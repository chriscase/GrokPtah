# Isolated Surface Proof Harness v0

This document inventories the current `main` Computer Use / isolated-guest code,
defines the synthetic proof harness added in this slice, and maps it to the Sep 18
2026 physical Mac gate and Contained Browser fallback. It does **not** claim
packaged Virtualization.framework qualification from Linux CI or simulator evidence.

Related issues: [#288](https://github.com/chriscase/GrokPtah/issues/288) (isolated
visual), [#286](https://github.com/chriscase/GrokPtah/issues/286) (agent-owned
surface), [#267](https://github.com/chriscase/GrokPtah/issues/267) (epic).

## Exact-main inventory (base `f11318828ba9720f7f018f037aa23293cd3b3e47`)

### Already satisfies Windowed Coding Run noninterference (semantic macOS path)

| Area | Location | Contract satisfied |
|---|---|---|
| Computer Run state machine | `grokptah-agent-bridge/src/computer_use/types.rs` | `AwaitingAuthorization → Ready → Observing/Acting → terminal`; `Stopped` / `UncertainOutcome` dispositions |
| Stop / cancel authority | `computer_use/service.rs` | Cancellation wins over in-flight action; late completion → `uncertain` |
| Restart recovery | `computer_use/store.rs` | Active runs → `interrupted`; claimed receipts → `uncertain`; grants cleared |
| Host shutdown join | `tests/host_shutdown_ownership.rs` | Computer-agent ops joined before lock release |
| Release gate adversarial | `tests/computer_use_release_gate.rs` | Injection, sensitive obs, drift, permission revoke |
| Visible activity (#286 UI) | `desktop/src/lib/computerActivity.ts` | Disposition-first activity mapping |
| Threat model honesty | `docs/COMPUTER_USE_THREAT_MODEL.md` | #288 disabled until separate input surface |

### Fails or is absent for Windowed Coding Run (#288 / physical isolation)

| Gap | Status on main before this slice |
|---|---|
| Guest lifecycle `NotStarted → Booting → Ready → Acting → Stopping → Destroyed` | **Absent** — no isolated guest crate |
| Host sentinel noninterference hooks (pointer/foreground/clipboard/unrelated window) | **Absent** — no testable sentinel registry |
| Synthetic proof harness for launch → boot → frame → inject → changed frame → Stop → destroy | **Absent** |
| Channel destroy / leak assertions after Stop | **Absent** |
| Crash/restart with no auto-retry after uncertain guest inject | Partial on Computer Run ledger only; **not** on guest surface |
| Virtualization.framework adapter | **Absent** — explicitly disabled in threat model |
| Agent-owned in-GrokPtah cursor / surface events (#286) | **Absent** — UI activity only |
| Packaged VM / notarized helper proof | **Absent** |

Draft PR archaeology (#447 `grokptah-isolated-visual`) is **not** merged wholesale. This slice
reconstructs only the contract and synthetic machinery current-main tests prove is missing.

## What this slice adds

| Deliverable | Location |
|---|---|
| `GuestLifecycle` state machine | `crates/codegen/grokptah-isolated-surface/src/lifecycle.rs` |
| Host sentinel registry + assertions | `crates/codegen/grokptah-isolated-surface/src/sentinel.rs` |
| Channel destroy registry | `crates/codegen/grokptah-isolated-surface/src/channels.rs` |
| Synthetic guest + harness orchestrator | `simulator.rs`, `harness.rs` |
| Restart snapshot for recovery tests | `store.rs` |
| Crash/Stop regression suite | `grokptah-isolated-surface/tests/stop_regression.rs` |
| Bridge fail-closed seam | `grokptah-agent-bridge/src/computer_use/isolated_surface.rs` |
| Bridge integration tests | `grokptah-agent-bridge/tests/isolated_surface_proof_harness.rs` |

### Lifecycle phases

```text
NotStarted → Booting → Ready → Acting → Stopping → Destroyed
                              ↘ Uncertain disposition (fail-closed, inject fenced)
```

- **Uncertain** is a disposition, not a resumable phase. It is recorded after possible
  guest input when outcome cannot be established (crash mid-inject, scheduled uncertainty,
  restart while `guest_input_possible`).
- **Stop** sets `inject_fenced` and is authoritative; further inject is rejected.
- **No auto-retry** after uncertain inject — explicit retry increments a counter and fails
  with `AutoRetryForbidden`.

### Host sentinels

`HostSentinelSnapshot` captures baseline pointer, foreground app/window, clipboard digest,
and an unrelated host window. The synthetic harness asserts these are unchanged across boot,
frame observe, inject, Stop, and destroy. `HostSentinelRegistry::refresh_from_host` is the
extension point for native Mac evidence collection on physical proof day.

### Evidence class

All harness output is `ProofEvidenceClass::SyntheticHarnessIneligible`. This is explicitly
**not** `VirtualizationFramework` and must not be cited as VM qualification.

Bridge admission `isolated_surface_admission_available()` remains **false**.

## Sep 18 2026 physical proof checklist mapping

| Physical Mac step | Synthetic harness equivalent | Native extension point |
|---|---|---|
| Launch isolated surface | `IsolatedSurfaceHarness::boot()` | VF helper spawn + window attach |
| Boot guest | `SyntheticGuest::boot` → `GuestLifecyclePhase::Ready` | Guest image boot + first frame |
| Capture frame | `observe_frame()` | ScreenCaptureKit / guest framebuffer channel |
| Inject ONE guest-local action | `inject_guest_action(ClickGuestButton)` | Guest input channel only |
| Changed frame | `FrameDelta.guest_local_change == true` | Frame digest / epoch increment |
| Stop | `stop()` → `GuestLifecycleDisposition::Stopped` | Operator Stop + helper teardown |
| Destroy channels | `ChannelRegistry::destroy_all` + `assert_all_destroyed` | Close VF/frame/input IPC |
| Host sentinels unchanged | `HostSentinelRegistry::assert_unchanged` | AX/CGEvent/clipboard/window probes |
| Crash mid-inject → uncertain, no retry | `stop_regression` tests | Same policy on native adapter |
| Process restart recovery | `recover_after_restart` + snapshot | Durable guest ledger on disk |

### Gate verdict

- **PASS (Sep 18):** real Mac completes the checklist with `ProofEvidenceClass::VirtualizationFramework`
  and unchanged host sentinels.
- **MISS → Contained Browser:** honest label; never call foreground `CGEvent` injection "isolated".

## Verification commands

```sh
# Format + lint (agent-bridge workspace)
cargo fmt --manifest-path crates/codegen/grokptah-agent-bridge/Cargo.toml --all
cargo clippy --locked --manifest-path crates/codegen/grokptah-agent-bridge/Cargo.toml \
  --all-targets -- -D warnings

# Focused harness tests
cargo test --locked --manifest-path crates/codegen/grokptah-agent-bridge/Cargo.toml \
  --test isolated_surface_proof_harness -- --test-threads=1

cargo test --locked --manifest-path crates/codegen/grokptah-isolated-surface/Cargo.toml \
  --test stop_regression -- --test-threads=1
```

## Residuals (honest, post-slice)

- No Virtualization.framework adapter, signed guest image, or packaged helper.
- No TCC entitlement or notarization claims.
- No Windows/Linux isolated surface.
- No agent-owned cursor / surface-event stream (#286 UI layer still disposition-only).
- No bridge admission enablement — `isolated_surface_admission_available()` stays false.
- Linux CI proves contract only; physical Mac proof is a separate exact-head gate.

## Non-claims

- Simulator / Linux CI does **not** qualify a packaged VM.
- Synthetic harness success does **not** enable isolated visual Computer Use in production.
- Contained Browser fallback is not implemented in this slice; only documented as the honest miss path.
