# Adaptive Computer Use

GrokPtah has one Computer Use control plane with three cost profiles:

| Profile | Model observation | Model calls | Repairs | Pointer/key actions |
| --- | --- | ---: | ---: | --- |
| Economy | bounded semantic structure, 48 controls / 24 KiB | 16 | 1 | no / no |
| Balanced | semantic structure plus bounded geometry, 256 controls / 128 KiB | 48 | 2 | yes / no |
| High Assurance | semantic structure, geometry, and an authenticated visual route, 1024 controls / 512 KiB | 96 | 3 | yes / yes |

These are efficiency budgets, not safety levels. All profiles use the existing
Computer Use target, consent, authority grant, effect-lease, redaction,
isolation, retry, freshness, and uncertainty checks. The profile validator
always runs after the profile-independent typed proposal validator and before
the existing `ComputerPolicy` dispatch check. Economy can reject more work; it
cannot admit work the safety path rejects.

`efficient` and `frontier` are ingest-only compatibility aliases for `economy`
and `high_assurance`. Serialization and new UI state contain only the three
canonical names.

## Decision and evidence

`AdaptivePolicyEngine` is deterministic. It uses explicit task/operator policy,
risk, model capabilities, observation confidence, ambiguity, and recovery
signals. Routine, consequential, and destructive tasks have Economy, Balanced,
and High Assurance risk floors respectively. A capability ceiling is calculated
from measured, route-bound evidence. Unknown, malformed, unsupported, or
synthetic-only evidence never becomes live eligibility; a risk floor above the
ceiling stops the run.
Volatile session measurements are diagnostic evidence only and cannot substitute
for durable measured authority.

Run-scoped adaptive state is persisted in the existing `ComputerRun` record.
The record includes bounded profile transitions, observation IDs and opaque
structural digests, decision reasons, capability snapshot references, typed
proposal/result evidence, recovery state, latency, and provider-reported usage
when available. Provider cost is never estimated.

The state is consumed through a private adapter seam for the host-issued
principal generation from #477, capability/effect-lease generation from #458,
and physical provider-attempt authority from #478. The seam has separate
pre-send admission and post-send settlement stages. Only the future physical
transport can produce acknowledgment, usage, latency, or an uncertain outcome;
the adaptive layer never authors or deserializes those values. No fallback
authority is minted here, and no public installation hook exists. Until the
canonical host assembly supplies the seam, adaptive live proposals stop with
`authority_unavailable`.

Semantic/headless observations use deterministic candidate ranking and bounded
summaries. Visual grounding uses a separate private adapter that accepts only
current redacted, integrity-checked evidence. Frame bytes, evidence tokens,
credentials, paths, clipboard/network secrets, raw policy documents, and
structural digests are excluded from the public run projection.

## Verification

The bridge integration test `tests/computer_use_adaptive.rs` covers malformed
and overconfident outputs, no unauthorized effects, profile transitions,
Economy safety invariance, crash/recovery cuts, visual grounding requirements,
and redacted projections. The hosted-style
`examples/computer_use_adaptive_campaign.rs` executes the production renderer,
validator, kernel, and `ReplayVerifier` over 4 fixtures × 5 adapters × 5
repeats × 3 profiles = 300 synthetic episodes. It uses no provider, socket,
screen, or dispatch and reports `eligibility: synthetic_only`.

The workflow `.github/workflows/computer-use-adaptive.yml` runs the focused
tests, campaign/replay verifier, and serial bridge suite. Live provider and
packaged hardware qualification remain separate gates and are deliberately
not inferred from the synthetic campaign.

## Qualification gaps

- The canonical #477 principal-generation adapter is not present on this base.
- The canonical #478 physical provider-attempt transport is not present on
  this base; the adaptive boundary therefore fails closed rather than
  self-attesting a provider receipt.
- No independent postcondition verifier or isolated guest is installed by the
  current desktop host, so High Assurance is unreachable in the default build.
- No live small-text, small-multimodal, or frontier provider campaign has run.
- No assembled macOS semantic Economy or isolated-visual High Assurance task has
  qualified hardware/TCC behavior.
