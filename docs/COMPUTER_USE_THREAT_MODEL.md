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
| AX action name or bundle identity treated as proof of background safety | No inference or allowlist upgrade. The native background backend is created only by a reversible local calibration receipt bound to one exact selection, process/window, generation, and element digest | `measured_background_text_entry_is_reversible_one_use_and_exact`; typed background backend/proof checks in `types` and `service` |
| Background calibration changes the user's active app/window or pointer | Native code snapshots the frontmost process, active layer-zero window, and physical pointer around every probe/restore/runtime dispatch; any change is `uncertain_outcome` and mints no receipt | `background_probe_requires_disposable_ack_and_fails_closed_on_interference`; native shim `GPTCaptureUserInteractionState` |
| Probe value is left behind after error or ambiguous native return | Restore is attempted through the same exact measured path; receipt issuance requires a final exact observation of the original value. Failure remains uncertain and authorizes nothing | `restore_background_probe`; reversible fixture test |
| Receipt replay, target switch, element drift, or foreground transition | Two-minute receipt is consumed once and binds the exact selection/target/native identity/element digest; every action revalidates target/frame/tree and background state | one-use/exact and foreground-transition tests in `macos_observation`; active-target cockpit test |
| Background mode silently invokes, scrolls, selects, activates, or uses raw input | Candidate proof/grant advertises `text_entry` only; Rust backend and native shim both accept `set_value` only; no `CGEventPost`, clipboard, AppleScript, or shell path exists | `measured_native_background_proof_is_exact_and_text_entry_only`; native source-envelope tests; cockpit scope test |
| Duplicate mutation or conflicting request ID | Proven idempotent replay bound to caller principal and authority/control epochs; cross-principal replay and legacy unstamped receipts fail closed | `ComputerUseService` idempotency tests; durable store receipt tests |
| Two Agents contend for one physical input domain | Deterministic FIFO leases serialize observation and dispatch; one granted/dispatching lease owns the domain. Distinct independently attested simulator domains may proceed concurrently | `same_domain_agents_serialize_observation_and_physical_dispatch`; `independently_isolated_agent_domains_can_hold_capacity_together` |
| Work cancelled, reassigned, expired, or Agent spec revised | Exact Work/Attempt/claimant/Agent/spec/Lane/workspace authority is revalidated at authorization, queue, preparation, and injection; stale authority fails before backend input | host durable-identity test; service surface-dispatch tests; `OrchStore::with_active_computer_work_attempt` |
| Crash or expiry before/after physical injection | Prepared becomes `known_not_injected`; injected becomes `uncertain`; neither is automatically replayed. Second reopen is stable | `prepared_and_injected_agent_dispatches_recover_fail_closed_twice`; `lease_expiry_fences_known_not_injected_and_uncertain_dispatches`; dispatch-ID dedup test |
| Lease-ledger retention pressure | Replay-safe terminal leases age out or yield capacity oldest-first; active and `uncertain` dispatches are never removed to make room. A ledger full of unresolved uncertainty fails closed | `ordinary_terminal_surface_leases_make_room_for_new_work`; `uncertain_surface_leases_are_never_pruned_for_capacity`; `reopen_ages_out_only_replay_safe_terminal_surface_leases` |
| Uncertain physical outcome followed by another Agent | The exact physical conflict domain remains poisoned until explicit reconciliation; another Agent cannot observe or dispatch there, while an independently attested isolated domain remains usable | `uncertain_dispatch_poison_is_exact_to_its_physical_input_domain` |
| Stop/Take over versus in-flight action completion | Durable cancellation wins and late completion becomes `uncertain` without incrementing action count. The stacked native candidate no longer waits behind the action mutex: it signals only the exact Run's in-flight native action and checkpoints before Accessibility dispatch and through the bounded activation wait. A synchronous AX call already entered cannot be rolled back and remains uncertain | `ComputerUseService::cancellation_wins_over_an_inflight_action_completion`; `native_cancel_signals_inflight_action_without_waiting_for_action_gate`; `native_cancel_is_scoped_to_the_exact_run`; `native_service_takeover_returns_before_blocked_source_action`; native cancellation-signal and cockpit takeover tests |
| Cockpit closed, Lane switched/archived, preview captured, or another approval read | App shell retains an exact Run binding and emergency controls; unique discovery fails closed on ambiguity; previews are excluded; approvals are per Run. Non-foreground owners receive only an opaque revoking-control token | `cockpit_discovery_fails_closed_when_multiple_runs_need_exact_binding`; `one_shot_preview_cannot_become_the_app_owned_control_surface`; `background_owner_retains_only_out_of_band_emergency_control`; `PersistentComputerRuns` tests |
| App reload, session switch, bounded-journal eviction, or cursor expiry | The stacked app shell replays typed, redaction-safe events against the exact app-owned session/Run identity. Its cursor persists across reloads; an expired or initially truncated window becomes a sticky visible gap while the retained tail resumes. A gap never disables Stop or becomes “complete” later | `app_owned_event_replay_is_typed_cursor_addressed_and_owner_scoped`; `legacy_audit_rows_gain_typed_surface_events_only_at_projection`; `computerRunReplay` and `PersistentComputerRuns` tests |
| Agent-attention UI confused with the host pointer or used as a data leak | The stacked candidate derives an optional point only from the exact current observation/action, stores target-relative basis points, omits screen origin and semantic identity/content, and renders an app-owned marker. Manual proposals emit no agent marker; absent or out-of-bounds geometry fails to no positional point | `attention_point_is_normalized_without_screen_or_element_identity`; desktop model/manual proposal tests; `ComputerCockpit` agent-attention tests |
| Proposal rejected while a stale marker remains active | Rejection is bound to the exact current ready observation and proposal owner, records typed `approval_rejected`, removes the pending proposal, and carries no new attention point. Replay retains history but the live marker requires the current agent-origin approval | desktop model-proposal rejection test; `ComputerCockpit` approval correlation |
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
- The candidate now applies the least-privilege bearer-authority slice to MCP Computer **read**
  methods: role ceilings, immutable session/workspace grants, and fail-closed scope checks are
  enforced. Packaged hardware/TCC/takeover and production evidence remain separate release gates.
- Keep #288 isolated visual execution disabled until a backend provides a genuinely separate input
  surface; hidden windows, separate Spaces, and global `CGEvent` injection do not qualify. Stage 1
  only makes isolation a typed, host-enforced contract. Remaining stages: authenticated isolated
  helper/input domain; out-of-band preemptive takeover after native entry; semantic-first isolated
  visual fallback. A stacked candidate now renders a redaction-safe app-owned agent-attention
  marker without moving the OS pointer, but it is not an isolated visual input backend. Persistent
  Stop is present in the stacked app-owned surface candidate; both remain part of the packaged
  acceptance gate.

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
