# Isolated Surface Proof Harness v0

This document inventories the current `main` Computer Use / isolated-guest code,
defines the synthetic proof harness, and maps it to the Sep 5–18 2026 calendar
(Phase-1 packet 3: Mac VF backend behind SPI, feature-gated). It does **not**
claim packaged Virtualization.framework qualification from Linux CI or dry-run
artifacts.

Related issues: [#288](https://github.com/chriscase/GrokPtah/issues/288) (isolated
visual), [#286](https://github.com/chriscase/GrokPtah/issues/286) (agent-owned
surface), [#267](https://github.com/chriscase/GrokPtah/issues/267) (epic).

## Calendar (Sep 5–18 2026)

| Date window | Packet | Status on main |
|---|---|---|
| Sep 5 | Packet 1 — Harness v0 (#542) | **Landed** — lifecycle, sentinels, synthetic guest, fence-first Stop |
| Sep 6–12 | Packet 2 — Backend SPI + sequencer + Contained Browser stub (#543) | **Landed** — `IsolatedSurfaceBackend`, `Sep18NoModelProofSequencer`, main-checkout sentinel |
| Sep 6–12 | Packet 3 — Mac VF backend behind SPI, feature-gated | **This slice** — `VirtualizationFrameworkBackend`, VF dry-run path, Mac sentinel hooks |
| Sep 18 | Physical Mac gate | VF PASS or honest Contained Browser pivot |

## Exact-main inventory (base `5b1b425fc8fb0d3c6f03495620019ed6b7287b05` + packet 3)

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

### Packet 1 (#542) + Packet 2 + Packet 3 deliverables

| Deliverable | Location |
|---|---|
| `GuestLifecycle` state machine | `crates/codegen/grokptah-isolated-surface/src/lifecycle.rs` |
| Host sentinel registry + main-checkout fence | `sentinel.rs` — `MainCheckoutFence` digest/mtime hook |
| Mac host sentinel collector hook | `sentinel.rs` — `MacHostSentinelCollector` + `refresh_from_host` |
| `IsolatedSurfaceBackend` SPI | `backend.rs` — boot / observe_frame / inject_guest_local / stop_fence_first / destroy |
| Synthetic backend (SPI impl) | `simulator.rs` — `SyntheticGuest` |
| Contained Browser stub (fail-closed) | `contained_browser.rs` — honest `ContainedBrowser` label, not PASS |
| VF backend stub (Mac + `vf-backend` feature) | `vf_backend.rs` — honest `VirtualizationFramework` label, dry-run only |
| VF dry-run sequencer path | `vf_dry_run.rs` + `Sep18NoModelProofSequencer::run_vf_dry_run` |
| Sep 18 no-model proof sequencer | `proof_sequencer.rs` — checklist + bounded fault matrix |
| Harness orchestrator | `harness.rs` — backend-generic, `with_vf_backend(receipt)` only for VF label |
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

**Mac physical proof hooks:**

1. Capture baseline before launch with `MacHostSentinelCollector::collect()` (stub fails closed until wired).
2. At boot, inject, and Stop probe points, call `IsolatedSurfaceHarness::refresh_host_sentinels(snapshot)` which delegates to `HostSentinelRegistry::refresh_from_host`.
3. `StopEvidence.host_sentinels_unchanged` remains authoritative only when probes match baseline.

### Evidence class (`ProofEvidenceClass`)

| Variant | Meaning |
|---|---|
| `Synthetic` | Harness/simulator output — ineligible for VF qualification |
| `VirtualizationFramework` | Reserved for real Mac VF physical proof PASS only |
| `ContainedBrowser` | Honest Sep 18 pivot label — stub fails closed, not current PASS |

Labels are fixed at backend creation and must **never** be upgraded at seal time.
Linux CI never produces `VirtualizationFramework` PASS. VF dry-run artifacts carry
`VF_DRY_RUN_NONCLAIM` and set `physical_pass_claimed: false`.

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

### VF dry-run path (`run_vf_dry_run`)

Rehearses the Sep 18 VF gate without claiming physical PASS:

| Platform | Outcome | Boot attempted |
|---|---|---|
| Linux CI / non-macOS | `UnsupportedPlatform` | No |
| macOS, `vf-backend` off | `FeatureDisabled` | No |
| macOS + `vf-backend` | `BackendUnavailable` (fail-closed stub) | Yes — boot returns `BackendUnavailable`, Stop tears down |

Requires `VfLaunchReceipt` with non-empty `physical_mac_proof_id`. Wire the backend only via `IsolatedSurfaceHarness::with_vf_backend(receipt)` — `with_backend` rejects VF without receipt.

## Sep 18 2026 physical proof checklist (Mac worker)

Run once a physical Mac worker is available. Admission stays **false** until a
separate gate enables it after honest PASS.

### Prerequisites

- macOS worker with Apple Silicon or Intel VT support
- Build with VF backend enabled:
  ```sh
  cargo test --locked --manifest-path crates/codegen/grokptah-isolated-surface/Cargo.toml \
    --features vf-backend --test proof_sequencer -- --test-threads=1
  ```
- Signed guest image + packaged helper (not in this slice)
- Operator Stop authority

### Checklist steps

| Step | Action | Pass criterion |
|---|---|---|
| 1 | Capture host baseline | `MacHostSentinelCollector::collect()` → store snapshot |
| 2 | Arm harness | `IsolatedSurfaceHarness::with_vf_backend(baseline, backend, receipt)` |
| 3 | Launch + boot guest | `boot()` → `Ready`, first frame epoch > 0 |
| 4 | Probe sentinels | `refresh_host_sentinels(collect())` — no drift |
| 5 | Capture frame | `observe_frame()` |
| 6 | Inject ONE guest-local action | `inject_guest_action(ClickGuestButton)` — guest-local change only |
| 7 | Verify postcondition | Frame digest/epoch changed; host sentinels still match baseline |
| 8 | Operator Stop | `stop()` — fence-first, channels destroyed |
| 9 | Final sentinel probe | `StopEvidence.host_sentinels_unchanged == true` |
| 10 | Reject stale inject | Post-Stop inject → `InjectFenced` |
| 11 | Seal evidence | `ProofEvidenceClass::VirtualizationFramework`, `physical_pass_claimed: true` (physical runner only — not dry-run) |

**MISS → Contained Browser:** if step 3–7 cannot complete by Sep 18, pivot to
`ContainedBrowserBackend` (stub fails closed today).

### Gate verdict

- **PASS (Sep 18):** real Mac completes the checklist with `ProofEvidenceClass::VirtualizationFramework`
  and unchanged host sentinels.
- **DRY-RUN (this slice):** `run_vf_dry_run` — honest nonclaim, `physical_pass_claimed: false`.
- **MISS → Contained Browser:** `ContainedBrowserBackend` stub documents the pivot path.

## Verification commands

```sh
# Format + lint (agent-bridge workspace)
cargo fmt --manifest-path crates/codegen/grokptah-agent-bridge/Cargo.toml --all
cargo clippy --locked --manifest-path crates/codegen/grokptah-agent-bridge/Cargo.toml \
  --all-targets -- -D warnings

# Focused harness tests (Linux CI — default features)
cargo test --locked --manifest-path crates/codegen/grokptah-agent-bridge/Cargo.toml \
  --test isolated_surface_proof_harness -- --test-threads=1

cargo test --locked --manifest-path crates/codegen/grokptah-isolated-surface/Cargo.toml \
  --test stop_regression -- --test-threads=1

cargo test --locked --manifest-path crates/codegen/grokptah-isolated-surface/Cargo.toml \
  --test proof_sequencer -- --test-threads=1

# Mac VF dry-run rehearsal (physical worker only)
cargo test --locked --manifest-path crates/codegen/grokptah-isolated-surface/Cargo.toml \
  --features vf-backend --test proof_sequencer sep18_vf_dry_run -- --test-threads=1
```

## Residuals (honest, post-packet-3)

- VF backend is dry-run stub only — no signed guest image, live VF IPC, or packaged helper.
- No TCC entitlement or notarization claims.
- No Windows/Linux isolated surface.
- No agent-owned cursor / surface-event stream (#286 UI layer still disposition-only).
- No bridge admission enablement — `isolated_surface_admission_available()` stays false.
- Contained Browser stub is fail-closed — not a PASS path.
- Linux CI proves contract + VF dry-run unsupported; physical Mac proof is a separate exact-head gate.

## Non-claims

- Simulator / Linux CI does **not** qualify a packaged VM.
- VF dry-run does **not** claim Sep 18 physical PASS.
- Synthetic harness success does **not** enable isolated visual Computer Use in production.
- `ContainedBrowser` stub does **not** implement browser isolation — only documents the Sep 18 pivot.
