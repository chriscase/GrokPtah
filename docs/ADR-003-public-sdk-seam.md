# ADR-003: A contract-only public SDK seam

**Status:** Accepted
**Date:** 2026-08-26
**Supersedes:** nothing. Executes ADR-002 §7 step 2 and ADR-002 §3's required
capability-advertisement contract.

## Context

GrokPtah has two hosts over one runtime — the Tauri desktop and
`grokptah-service` — and at least one prospective third consumer in ContextDesk.
A third consumer today has two ways in, both bad:

* depend on `grokptah-agent-bridge`, which is the runtime (keyring, Axum control
  plane, `reqwest`, provider profiles, durable stores) and would make every
  internal type part of that consumer's compatibility surface; or
* hand-roll a JSON-RPC client against the `ptah_*` tools, re-deriving the DTOs,
  error mapping, cursor rules, and retry policy per consumer.

ADR-002 §7 fixes the ordering for reusable behavior and explicitly gates crate
extraction (step 4) on two named running consumers executing a shared parity
matrix, and SDK publication (step 5) on a named compatibility owner maintaining
that matrix for a real external consumer. Neither gate is met.

ADR-002 §3 separately records that "explicit capability advertisement is a
required future contract", which "must define stable capability identifiers, the
host/version that asserted them, attempt-time capture, and typed
unsupported/forbidden failures."

## Decision

Introduce `crates/codegen/grokptah-agent-sdk`: a **contract-only** crate holding
the versioned, provider-neutral capability boundary — traits, DTOs, a stable
error taxonomy, a capability document, pagination, a deterministic fake, and an
adapter-agnostic conformance battery.

Four constraints define what this is and is not:

1. **Nothing is extracted.** No behavior moves out of `grokptah-agent-bridge`;
   the bridge is unchanged. The SDK has no dependency on it, in either
   direction. This is ADR-002 §7 **step 2**, not step 4.
2. **Nothing is published.** The crate is `publish = false`. Consumers depend on
   it by path or git revision until ADR-002 §7 step 5 is met.
3. **No second lifecycle machine.** `RunLifecycle`, `StopCause`,
   `FollowUpDisposition`, and the tool enums mirror the runtime's own types
   exactly, with pinned wire tokens. A runtime state change is a contract major
   change, never an SDK-side translation.
4. **Existing denial rules are preserved and made explicit.** Computer Use
   *control* and provider credentials are permanently forbidden capability
   identifiers, stamped by `CapabilityDocument::new` itself so an adapter cannot
   advertise them by mistake. They are typed, discoverable denials rather than
   silent absence.

The capability document satisfies ADR-002 §3: stable dotted identifiers, the
asserting host kind/product/version, an assertion timestamp suitable for
attempt-time capture, and three-valued availability mapping to typed
`unsupported` / `forbidden_scope` failures — with a fourth, distinct
`capability_unavailable` for an identifier the host never mentioned.

## Consequences

### What this unlocks

* A consumer can build and test an entire UI against `FakeControlPlane` with no
  GrokPtah process running, no provider calls, and no credentials.
* The parity matrix ADR-002 §7 step 3 requires now exists as executable checks
  (`conformance::run_battery`) rather than prose, and reports unrunnable checks
  as **skipped** rather than passed.
* Adding a real adapter is additive: implement one trait, run the battery.

### What this costs

* One more crate and one more CI step.
* A second place that must change when a runtime lifecycle state, error code, or
  bound changes. The pinned wire-token tests are how that stays honest: they
  fail loudly rather than drifting.

### What remains gated

* **Step 3** is partly done. The battery now runs against two adapters — the
  deterministic fake and the service adapter over a scripted `ptah_*`
  transport — and a test asserts the two agree wherever both can run a check.
  Neither is a *live* host. Running it against a running `grokptah-service` and
  the desktop's embedded control server, in CI, with stated pass criteria, is
  what step 3 actually asks for.
* **Step 4** requires two named running consumers on the same boundary.
  ContextDesk counts only once it exercises this in running code.
* **Step 5** requires a named compatibility and version owner.

### Boundaries this preserves

The SDK carries the **public projection** only. Operator/admin surfaces
(permission decisions, credential management, provider profiles, Computer Use
grants, run approval and promotion) are absent by type, not filtered at the
transport. Authority-owned secrets never cross; the single secret on the
boundary — a work lease token — is held in a non-`Serialize`, `Debug`-redacted
type. Transcript content and absolute host paths have no representation.

The service adapter over the authenticated MCP control plane landed under
this decision. It maps ten `ptah_*` tools and explicitly declines the manager,
managed-execution, and approval/promotion surfaces: those are where mutation
grants are issued and where the newest active line is still moving.

Full design, adapter mapping, versioning rules, and residual P1/P2 work:
[`AGENT_SDK_SEAM.md`](AGENT_SDK_SEAM.md).
