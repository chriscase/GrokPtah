# Computer Use Threat Model and Release Gate

This document is the evidence index for [#274](https://github.com/chriscase/GrokPtah/issues/274).
It describes what the first macOS Computer Run proves, what remains hardware- or packaging-dependent,
and what is deliberately unsupported. It does not turn an unsupported behavior into a hidden fallback.

## Trust boundaries

```text
untrusted app/window content, screenshots, accessibility labels, model output, MCP input
                              |
                              v
       bridge-owned typed run + policy + exact target/observation/grant checks
                              |
             +----------------+----------------+
             |                                 |
       local operator UI                  platform adapter
       approval / Stop / Take over        ScreenCaptureKit + AX
             |                                 |
             +---------------+-----------------+
                             v
                 bounded semantic action only
```

The bridge owns authority, state, idempotency, freshness, limits, and audit metadata. Tauri is an
OS adapter and the React cockpit is a projection of state. The model never receives a grant, native
dispatch handle, host path, screenshot asset locator, credential, or general shell/MCP tool.

## Evidence matrix

| Threat or scenario | Current disposition | Authoritative evidence |
|---|---|---|
| Instruction-shaped text in an observation | Proven deterministically; content remains data and cannot create a target or action scope | `computer_use_release_gate::observed_instruction_text_cannot_expand_action_scope`; `computer_agent` proposal parser tests |
| Sensitive/secure element or unredacted screenshot | Proven fail-closed before exposure or dispatch | `computer_use_release_gate::sensitive_observation_fails_before_model_visible_action_or_dispatch`; policy tests; native observation tests |
| Target/window generation drift during observation | Proven fail-closed; the run becomes `failed` and authority is revoked | `computer_use_release_gate::observation_target_drift_fails_inflight_run_and_revokes_authority` |
| Stale semantic element or geometry/action mismatch | Proven by exact observation, target, element, enabled-state, advertised-action, and bounds checks | policy tests; service stale-observation tests; native action tests |
| Permission revoked during action | Proven fail-closed; in-flight action fails and authority is cleared | `computer_use_release_gate::permission_revocation_fails_action_and_clears_authority`; native permission-revocation tests |
| Unsupported host pointer/coordinate fallback | Explicitly unsupported in the first slice; service rejects it before backend dispatch | `computer_use_release_gate::unsupported_pointer_fallback_never_reaches_backend`; proposal schema excludes pointer/key actions |
| Duplicate mutation or conflicting request ID | Proven idempotent replay and conflict rejection | `ComputerUseService` idempotency tests; durable store receipt tests |
| Stop/Take over versus in-flight action completion | Proven that cancellation wins and late completion becomes `uncertain` without incrementing action count | `ComputerUseService::cancellation_wins_over_an_inflight_action_completion`; desktop cockpit takeover tests |
| Restart during a run or mutation | Proven: active runs become `interrupted`, grants/observations clear, and claimed receipts become `uncertain` | durable store restart tests |
| Evidence size, retention, integrity, and path leakage | Proven bounded evidence, hash/length verification, atomic records, retention, and opaque asset IDs | service evidence tests; store retention/integrity tests; `docs/COMPUTER_USE.md` |
| Model/provider change during Computer inference | Proven at the model boundary; ephemeral qualification is cleared and late proposals are rejected | `computer_agent` and desktop proposal revalidation tests |
| Malicious app mimicking a reviewed title/icon | Native target attestation is required; packaged identity and hardware proof remain release-gate work | `docs/COMPUTER_USE_MACOS.md`; #274 hardware-backed smoke |
| Focus theft, app restart, window reuse, resize, DPI/display change, and occlusion | Native fixtures cover the implemented identity/geometry checks; broader hardware matrix remains required before stable release | macOS observation/action tests; #274 manual exact-head matrix |
| Credential UI, lock/login, permission panels, privilege prompts, password managers | Explicit unsupported/denied disposition; no model-visible capture or action is allowed | `Sensitivity::{Secure,SystemRestricted}` policy; native surface classification tests |
| MCP Computer mutations, raw shell, clipboard, AppleScript, unattended control | Explicitly unsupported in this release; #271 is a later scoped interoperability phase | `CONTROL_TOOLS` boundary; `docs/COMPUTER_USE.md` non-goals |
| Windows/Linux native control | Explicitly unsupported until their platform issues have native consent and evidence | #275 and #276 |

## Packaged identity and cleanup accounting

| Threat | Mitigation | Evidence |
| --- | --- | --- |
| A bundle vouches for its own signature via a text file it ships | Signing facts come only from a `CodeIdentityProbe` running pinned `codesign`/`spctl`; self-attestation filenames are recorded and never read | `packaged_authority::tests::a_planted_codesign_text_file_cannot_change_the_verdict`, `adversarial_matrix::a_bundle_local_attestation_file_is_recorded_and_never_read` |
| The expected designated requirement is synthesized from the observed Team ID, making admission a tautology | Expectations load from an operator trust root outside the artifact root; the requirement is compared for exact equality and is never formatted from an observation | `trust_root::tests::*`, `adversarial_matrix::synthesized_designated_requirement_and_team_identity_are_refused` |
| Negated signing text is read as a positive verdict | Classification reads only anchored `Key=Value` lines and refuses values carrying a negation token; notarization comes from Gatekeeper's `source=` | `code_identity::tests::negated_values_cannot_invert_into_a_positive_class`, `adversarial_matrix::negated_signing_text_cannot_invert_into_admission` |
| A missing or symlinked entitlements plist silently defaults to a synthesized digest | `hash_file` fails closed on symlinks and non-files; there is no fallback digest | `adversarial_matrix::symlinked_entitlements_fail_closed` |
| A discarded deletion error makes teardown look complete | Cleanup receipts are re-observed from the filesystem, guest handle, and occupancy store after teardown; unresolved cleanup is surfaced as `UncertainOutcome` and the guest is not marked clean | `adversarial_matrix::cleanup_that_leaves_a_resource_behind_is_uncertain` |
| A fabricated receipt claims a clean teardown | Per-resource and whole-receipt digests must recompute, and the outcome must agree with the probe set | `adversarial_matrix::a_fabricated_cleanup_receipt_does_not_validate` |
| A second helper-local state machine disagrees with the host about what was injected | Exactly one host-owned authority; a CI gate fails the build if a second one reappears | `scripts/check-adversarial-reachable.sh` |
| A record that deserializes but is semantically invalid is trusted | Such records are quarantined on open alongside unreadable ones | `adversarial_matrix::a_torn_lease_record_is_quarantined_not_trusted` |
| An unreadable occupancy record reads as free | Occupancy reads fail closed; unreadable means occupied | `occupancy::tests::a_corrupt_record_denies_rather_than_reading_as_clear` |
| Injected input is replayed after a crash | Injected is durable before injection; a failed durable write refuses the dispatch, a failed post-injection write is Uncertain, and restarts never replay | `adversarial_matrix::two_restarts_after_injection_never_replay` |

## Release blockers still open

- Run the three-action disposable macOS fixture proof through the packaged GrokPtah identity with
  Screen Recording and Accessibility grants. Terminal-owned grants do not prove packaged identity.
- Complete the named hardware matrix for focus/geometry/display changes and permission revocation.
- Keep #271 Computer MCP mutations disabled until the shared event/approval contract and its threat
  review are complete.
- Keep #288 isolated visual execution disabled until a backend provides a genuinely separate input
  surface; hidden windows, separate Spaces, and global `CGEvent` injection do not qualify.

## Verification commands

```sh
# Computer Use release gate (agent bridge)
cargo test --locked --manifest-path crates/codegen/grokptah-agent-bridge/Cargo.toml \
  --test computer_use_release_gate -- --test-threads=1

# Packaged authority: unit suites plus the adversarial matrix. This crate is its
# own workspace root, so the bridge-wide `cargo test` does NOT run these.
cargo test --locked --manifest-path crates/codegen/grokptah-isolated-visual/Cargo.toml \
  -- --test-threads=1

# Prove those suites are still reachable and no second authority reappeared.
scripts/check-adversarial-reachable.sh

# Every committed lockfile still resolves from this exact tree.
scripts/check-committed-lockfiles.sh
```
