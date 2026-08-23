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
       + capability proof, principal, surface incarnation, freshness fence
       + durable WorkAttempt lease and physical-dispatch fence
                              |
             +----------------+----------------+
             |                                 |
       local operator UI                  platform adapter
       approval / Stop / Take over        ScreenCaptureKit + AX
             |                                 |  foreground-semantic only
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
| Unsupported host pointer/coordinate fallback | Explicitly unsupported; pointer/key require an independently isolated input-domain proof even if legacy booleans or blanket grant classes are true | `computer_use_release_gate::unsupported_pointer_fallback_never_reaches_backend`; policy `pointer_and_key_require_isolated_proof_even_with_grant_class`; typed `ComputerCapabilityProof` |
| Legacy/missing isolation fields | Deserialize to unproven or foreground-only; cannot authorize background, isolated, pointer, or key actions | `types` hydrate/from_wire tests; store restart coerces isolated/background proof to unproven |
| Principal mismatch (session or Agent) | Denied before grant, observation, action, evidence read, and takeover. Agent authority is issued only after `AgentHost` resolves the exact active WorkAttempt, assigned Agent, and current spec revision; public Agent-shaped strings remain unusable | policy and `ComputerUseService` principal-mismatch tests; `host::computer_agent_host_tests::agent_computer_run_admission_resolves_exact_durable_work_identity` |
| Surface/incarnation/epoch/frame/freshness mismatch | Denied before backend dispatch; wall-clock alone is not proof | policy surface/epoch/freshness tests |
| Simulator isolated fixture used as native isolation | Simulator-only origin; native macOS backends cannot carry isolated proof | simulator fixture tests; `ComputerRun::new_with_isolation` native stamp tests |
| Per-window IDs treated as isolated input domains | Native macOS interns one host-global-foreground conflict domain (capacity 1); distinct windows share freshness clocks | `macos_observation::native_macos_windows_share_one_host_global_foreground_conflict_domain` |
| Duplicate mutation or conflicting request ID | Proven idempotent replay bound to caller principal and authority/control epochs; cross-principal replay and legacy unstamped receipts fail closed | `ComputerUseService` idempotency tests; durable store receipt tests |
| Two Agents contend for one physical input domain | Deterministic FIFO leases serialize observation and dispatch; one granted/dispatching lease owns the domain. Distinct independently attested simulator domains may proceed concurrently | `same_domain_agents_serialize_observation_and_physical_dispatch`; `independently_isolated_agent_domains_can_hold_capacity_together` |
| Work cancelled, reassigned, expired, or Agent spec revised | Exact Work/Attempt/claimant/Agent/spec/Lane/workspace authority is revalidated at authorization, queue, preparation, and injection; stale authority fails before backend input | host durable-identity test; service surface-dispatch tests; `OrchStore::with_active_computer_work_attempt` |
| Crash or expiry before/after physical injection | Prepared becomes `known_not_injected`; injected becomes `uncertain`; neither is automatically replayed. Second reopen is stable | `prepared_and_injected_agent_dispatches_recover_fail_closed_twice`; `lease_expiry_fences_known_not_injected_and_uncertain_dispatches`; dispatch-ID dedup test |
| Lease-ledger retention pressure | Replay-safe terminal leases age out or yield capacity oldest-first; active and `uncertain` dispatches are never removed to make room. A ledger full of unresolved uncertainty fails closed | `ordinary_terminal_surface_leases_make_room_for_new_work`; `uncertain_surface_leases_are_never_pruned_for_capacity`; `reopen_ages_out_only_replay_safe_terminal_surface_leases` |
| Stop/Take over versus in-flight action completion | Proven that cancellation wins and late completion becomes `uncertain` without incrementing action count. Takeover is durable bookkeeping-safe, not physically preemptive inside the native action gate | `ComputerUseService::cancellation_wins_over_an_inflight_action_completion`; desktop cockpit takeover tests |
| Restart during a run or mutation | Proven: active runs become `interrupted`, grants/observations clear, and claimed receipts become `uncertain` | durable store restart tests |
| Evidence size, retention, integrity, and path leakage | Proven bounded evidence, hash/length verification, atomic records, retention, and opaque asset IDs | service evidence tests; store retention/integrity tests; `docs/COMPUTER_USE.md` |
| Model/provider change during Computer inference | Proven at the model boundary; ephemeral qualification is cleared and late proposals are rejected | `computer_agent` and desktop proposal revalidation tests |
| Malicious app mimicking a reviewed title/icon | Native target attestation is required; packaged identity and hardware proof remain release-gate work | `docs/COMPUTER_USE_MACOS.md`; #274 hardware-backed smoke |
| Focus theft, app restart, window reuse, resize, DPI/display change, and occlusion | Native fixtures cover the implemented identity/geometry checks; broader hardware matrix remains required before stable release | macOS observation/action tests; #274 manual exact-head matrix |
| Credential UI, lock/login, permission panels, privilege prompts, password managers | Explicit unsupported/denied disposition; no model-visible capture or action is allowed | `Sensitivity::{Secure,SystemRestricted}` policy; native surface classification tests |
| MCP Computer mutations, raw shell, clipboard, AppleScript, unattended control | Explicitly unsupported in this release; #271 is a later scoped interoperability phase | `CONTROL_TOOLS` boundary; `docs/COMPUTER_USE.md` non-goals |
| Windows/Linux native control | Explicitly unsupported until their platform issues have native consent and evidence | #275 and #276 |

## Release blockers still open

- Run the three-action disposable macOS fixture proof through the packaged GrokPtah identity with
  Screen Recording and Accessibility grants. Terminal-owned grants do not prove packaged identity.
  Packaged hardware focus, TCC, and takeover evidence remain explicitly unverified.
- Complete the named hardware matrix for focus/geometry/display changes and permission revocation.
- Keep #271 Computer MCP mutations disabled until the shared event/approval contract and its threat
  review are complete.
- MCP Computer **read** methods still depend on the separate least-privilege bearer-authority
  repair. Stage 1 is not stable or release-ready until that scoped authority slice lands.
- Keep #288 isolated visual execution disabled until a backend provides a genuinely separate input
  surface; hidden windows, separate Spaces, and global `CGEvent` injection do not qualify. Stage 1
  only makes isolation a typed, host-enforced contract. Remaining stages: authenticated isolated
  helper/input domain; out-of-band preemptive takeover after native entry; semantic-first isolated
  visual fallback; cockpit agent cursor and always-available Stop.

## Verification command

```sh
cargo fmt --check --manifest-path crates/codegen/grokptah-agent-bridge/Cargo.toml
cargo test --locked --manifest-path crates/codegen/grokptah-agent-bridge/Cargo.toml \
  --lib computer_use -- --test-threads=1
cargo test --locked --manifest-path crates/codegen/grokptah-agent-bridge/Cargo.toml \
  --lib computer_agent -- --test-threads=1
cargo test --locked --manifest-path crates/codegen/grokptah-agent-bridge/Cargo.toml \
  --lib mcp_control::tests::computer -- --test-threads=1
cargo test --locked --manifest-path crates/codegen/grokptah-agent-bridge/Cargo.toml \
  --test computer_use_release_gate -- --test-threads=1
cargo test --locked --manifest-path crates/codegen/grokptah-agent-bridge/Cargo.toml \
  --test mcp_streamable_transport live_computer -- --test-threads=1
cargo test --locked --manifest-path desktop/src-tauri/Cargo.toml \
  --lib computer_use -- --test-threads=1
```
