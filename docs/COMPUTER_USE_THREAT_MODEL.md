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

## Release blockers still open

- Run the three-action disposable macOS fixture proof through the packaged GrokPtah **helper**
  identity with Screen Recording and Accessibility grants. Terminal-owned grants and in-process
  host TCC do not prove packaged identity (#444).
- Assemble, sign, and notarize the declared helper at
  `Contents/Helpers/GrokPtah Computer Use Helper.app`. Empty entitlements and Info.plist on this
  branch declare the identity; they are not a notarized helper.
- Complete the named hardware matrix for focus/geometry/display changes and permission revocation.
- Keep #271 Computer MCP mutations disabled until the shared event/approval contract and its threat
  review are complete.
- Keep #288 isolated visual execution disabled until a backend provides a genuinely separate input
  surface; hidden windows, separate Spaces, and global `CGEvent` injection do not qualify.

## Verification command

```sh
cargo test --locked --manifest-path crates/codegen/grokptah-agent-bridge/Cargo.toml \
  --test computer_use_release_gate -- --test-threads=1
```
