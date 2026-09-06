# Isolated Surface Proof Harness v0

This document inventories the current `main` Computer Use / isolated-guest code,
defines the synthetic proof harness, and maps it to the Sep 5–18 2026 calendar
(Phase-1 packet 2: Backend SPI + no-model sequencer + Contained Browser stub).
It does **not** claim packaged Virtualization.framework qualification from Linux CI
or simulator evidence.

Related issues: [#288](https://github.com/chriscase/GrokPtah/issues/288) (isolated
visual), [#286](https://github.com/chriscase/GrokPtah/issues/286) (agent-owned
surface), [#267](https://github.com/chriscase/GrokPtah/issues/267) (epic).

## Calendar (Sep 5–18 2026)

| Date window | Packet | Status on main |
|---|---|---|
| Sep 5 | Packet 1 — Harness v0 (#542) | **Landed** — lifecycle, sentinels, synthetic guest, fence-first Stop |
| Sep 6–12 | Packet 2 — Backend SPI + sequencer + Contained Browser stub | **This slice** — `IsolatedSurfaceBackend`, `Sep18NoModelProofSequencer`, main-checkout sentinel |
| Sep 13–17 | Packet 3 (next) — Mac VF backend behind SPI, feature-gated | Planned — still `admission false` |
| Sep 18 | Physical Mac gate | VF PASS or honest Contained Browser pivot |

## Exact-main inventory (base `6a55faf9711addca01c1eec795850f11ba42bcec` + packet 2)

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

### Packet 1 (#542) + Packet 2 deliverables

| Deliverable | Location |
|---|---|
| `GuestLifecycle` state machine | `crates/codegen/grokptah-isolated-surface/src/lifecycle.rs` |
| Host sentinel registry + main-checkout fence | `sentinel.rs` — `MainCheckoutFence` digest/mtime hook |
| `IsolatedSurfaceBackend` SPI | `backend.rs` — boot / observe_frame / inject_guest_local / stop_fence_first / destroy |
| Synthetic backend (SPI impl) | `simulator.rs` — `SyntheticGuest` |
| Contained Browser stub (fail-closed) | `contained_browser.rs` — honest `ContainedBrowser` label, not PASS |
| Sep 18 no-model proof sequencer | `proof_sequencer.rs` — checklist + bounded fault matrix |
| Harness orchestrator | `harness.rs` — backend-generic, fence-first Stop preserved |
| Channel destroy registry | `channels.rs` |
| Restart snapshot for recovery tests | `store.rs` |
| Crash/Stop regression suite | `tests/stop_regression.rs` |
| Sequencer + SPI regression | `tests/proof_sequencer.rs` |
| Bridge fail-closed seam | `grokptah-agent-bridge/src/computer_use/isolated_surface.rs` |
| Bridge integration tests | `grokptah-agent-bridge/tests/isolated_surface_proof_harness.rs` |

### Lifecycle phases

```text
NotStarted → Booting → Ready → Acting → Stopping → Destroyed
                              ↘ Uncertain disposition (fail-closed, inject fenced)
```

- **Uncertain** is a disposition, not a resumable phase.
- **Stop is fence-first:** `begin_stop` + `backend.stop_fence_first()` before teardown.
- **No auto-retry** after uncertain inject.

### Host sentinels + main-checkout fence

`HostSentinelSnapshot` captures baseline pointer, foreground app/window, clipboard digest,
unrelated host window, and `MainCheckoutFence` (digest or mtime fence for the main checkout,
not the disposable worktree). Synthetic mutation of main-checkout fence is a regression
case in `proof_sequencer` tests.

### Evidence class (`ProofEvidenceClass`)

| Variant | Meaning |
|---|---|
| `Synthetic` | Harness/simulator output — ineligible for VF qualification |
| `VirtualizationFramework` | Reserved for real Mac VF physical proof PASS only |
| `ContainedBrowser` | Honest Sep 18 pivot label — stub fails closed, not current PASS |

Labels are fixed at backend creation and must **never** be upgraded at seal time.
Linux CI never produces `VirtualizationFramework` PASS.

Bridge admission `isolated_surface_admission_available()` remains **false**.

## Sep 18 no-model proof sequencer

`Sep18NoModelProofSequencer` runs the checklist against the synthetic backend:

1. **Arm** — verify `ProofEvidenceClass::Synthetic`
2. **Boot** — `IsolatedSurfaceHarness::boot()`
3. **Frame+challenge** — `observe_frame()`, epoch > 0
4. **One guest-local action** — `inject_guest_action` (marks possible before boundary)
5. **Postcondition** — guest-local frame change verified
6. **Stop/destroy** — fence-first Stop, channels destroyed
7. **Reject stale tokens** — post-Stop inject must return `InjectFenced`
8. **Seal evidence** — `SealedProofEvidence` with nonclaim string

### Bounded fault matrix

| Case | Scenario |
|---|---|
| `BootStop` | Boot then Stop before inject |
| `PreDispatchStop` | Boot, frame, Stop before guest-local dispatch |
| `LostAckUncertain` | Uncertain inject → Stop preserves Uncertain |
| `RestartNoReplay` | Restart after uncertain → destroyed, no inject replay |

## Sep 18 2026 physical proof checklist mapping

| Physical Mac step | Synthetic harness equivalent | Native extension point |
|---|---|---|
| Launch isolated surface | `IsolatedSurfaceHarness::boot()` | VF helper spawn + window attach |
| Boot guest | `IsolatedSurfaceBackend::boot` → `Ready` | Guest image boot + first frame |
| Capture frame | `observe_frame()` | ScreenCaptureKit / guest framebuffer channel |
| Inject ONE guest-local action | `inject_guest_local(ClickGuestButton)` | Guest input channel only |
| Changed frame | `FrameDelta.guest_local_change == true` | Frame digest / epoch increment |
| Stop | `stop()` fences first → teardown → probe | Operator Stop + helper teardown |
| Destroy channels | `ChannelRegistry::destroy_all` | Close VF/frame/input IPC |
| Host sentinels unchanged | `StopEvidence.host_sentinel_probe_error` separate | AX/CGEvent/clipboard/window + main-checkout probes |
| Crash mid-inject → uncertain, no retry | fault matrix + `stop_regression` | Same policy on native adapter |
| Process restart recovery | `RestartNoReplay` fault case | Durable guest ledger on disk |

### Gate verdict

- **PASS (Sep 18):** real Mac completes the checklist with `ProofEvidenceClass::VirtualizationFramework`
  and unchanged host sentinels.
- **MISS → Contained Browser:** `ContainedBrowserBackend` stub documents the pivot path; stub
  fails closed until real browser isolation lands — never call foreground `CGEvent` injection "isolated".

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

cargo test --locked --manifest-path crates/codegen/grokptah-isolated-surface/Cargo.toml \
  --test proof_sequencer -- --test-threads=1
```

## Residuals (honest, post-packet-2)

- No Virtualization.framework adapter, signed guest image, or packaged helper.
- No TCC entitlement or notarization claims.
- No Windows/Linux isolated surface.
- No agent-owned cursor / surface-event stream (#286 UI layer still disposition-only).
- No bridge admission enablement — `isolated_surface_admission_available()` stays false.
- Contained Browser stub is fail-closed — not a PASS path.
- Linux CI proves contract only; physical Mac proof is a separate exact-head gate.

## Non-claims

- Simulator / Linux CI does **not** qualify a packaged VM.
- Synthetic harness success does **not** enable isolated visual Computer Use in production.
- `ContainedBrowser` stub does **not** implement browser isolation — only documents the Sep 18 pivot.
