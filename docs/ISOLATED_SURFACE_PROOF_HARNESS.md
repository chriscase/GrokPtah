# Isolated Surface Proof Harness v0

This document inventories the current `main` Computer Use / isolated-guest code,
defines the synthetic proof harness, and maps it to the Sep 5–18 2026 calendar
(Phase-1 packet 4: Contained Browser substrate v0 behind SPI). It does **not**
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
| Sep 6–12 | Packet 3 — Mac VF backend behind SPI, feature-gated (#545) | **Landed** — `VirtualizationFrameworkBackend`, VF dry-run path, Mac sentinel hooks |
| Sep 6–12 | Packet 4 — Contained Browser substrate v0 (#546) | **Landed** — real browser lifecycle on simulator substrate, CB dry-run path, admission false |
| Sep 6–12 | Packet 5 — Sep 18 checklist runner + evidence-pack verifier (#547) | **Landed** — `grokptah-sep18-checklist` CLI, sealed pack + independent verifier |
| Sep 6–12 | Packet 6 — Harness Stop honesty (#548) | **Landed** — Stop cleanup survives disk/audit failure; `Destroyed` is confirmed-only |
| Sep 6–12 | Packet 7 — `stop_fence_first` no default (#TBD) | **This slice** — trait default removed; production adapters must fence with ack/failure |
| Sep 18 | Physical Mac gate | VF PASS or honest Contained Browser pivot |

## Exact-main inventory (base `e1b1cc9611993d6456e2b73b110d2b60397cf38f` + packet 7)

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

### Packet 1 (#542) + Packet 2 + Packet 3 + Packet 4 deliverables

| Deliverable | Location |
|---|---|
| `GuestLifecycle` state machine | `crates/codegen/grokptah-isolated-surface/src/lifecycle.rs` |
| Host sentinel registry + main-checkout fence | `sentinel.rs` — `MainCheckoutFence` digest/mtime hook |
| Mac host sentinel collector hook | `sentinel.rs` — `MacHostSentinelCollector` + `refresh_from_host` |
| `IsolatedSurfaceBackend` SPI | `backend.rs` — boot / observe_frame / inject_guest_local / stop_fence_first / destroy |
| Synthetic backend (SPI impl) | `simulator.rs` — `SyntheticGuest` |
| Contained Browser substrate v0 (simulator) | `contained_browser.rs` — honest `ContainedBrowser` label, browser-only lifecycle, not isolation PASS |
| Contained Browser dry-run sequencer path | `contained_browser_dry_run.rs` + `Sep18NoModelProofSequencer::run_contained_browser_dry_run` |
| Contained Browser fault matrix | `run_contained_browser_fault_matrix` — bounded fault cuts on CB substrate |
| Contained Browser regression | `tests/contained_browser_regression.rs` |
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
| Sep 18 checklist runner | `checklist_runner.rs` — default CB dry-run, optional VF dry-run / fault matrix |
| Sealed evidence pack + verifier | `evidence_pack.rs` — independent accept/reject with explicit codes |
| Checklist CLI | `src/bin/grokptah-sep18-checklist.rs` — `run` + `verify` subcommands |
| Evidence-pack verifier tests | `tests/evidence_pack_verifier.rs` — happy path + tamper rejection |

### Lifecycle phases

```text
NotStarted → Booting → Ready → Acting → Stopping → Destroyed
                              ↘ Uncertain disposition (fail-closed, inject fenced)
```

- **Uncertain** is a disposition, not a resumable phase.
- **Stop is fence-first:** `begin_stop` + `backend.stop_fence_first()` before teardown.
- **`stop_fence_first` has no trait default:** production adapters (Contained Browser, VF when feature-gated, any non-test SPI impl) must implement an explicit fence that acknowledges success or returns `BackendUnavailable`/other explicit error. A silent `Ok(())` default does not qualify as a fence.
- **Stop cleanup survives disk/audit failure:** `persist_snapshot` errors during Stop are recorded in `StopEvidence.persist_snapshot_error` but never skip channel/backend teardown.
- **`Destroyed` is confirmed-only:** lifecycle advances to `Destroyed` only when backend destroy succeeds (or was not required). A failed backend destroy leaves the surface in `Stopping` with `backend_destroy_error` set.
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
| `ContainedBrowser` | Honest Sep 18 pivot label — substrate v0 on simulator; not isolation PASS, not VF PASS |

Labels are fixed at backend creation and must **never** be upgraded at seal time.
Linux CI never produces `VirtualizationFramework` PASS. VF dry-run artifacts carry
`VF_DRY_RUN_NONCLAIM` and set `physical_pass_claimed: false`. Contained Browser
dry-run artifacts carry `CONTAINED_BROWSER_DRY_RUN_NONCLAIM` and set
`isolation_pass_claimed: false`.

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

### Contained Browser dry-run path (`run_contained_browser_dry_run`)

Rehearses the Sep 18 pivot substrate without claiming isolation or VF PASS:

| Platform | Outcome | Checklist completed |
|---|---|---|
| Linux CI / default (simulator substrate) | `SubstrateRehearsal` | Yes — full checklist on in-process browser simulator |
| Any, `browser-engine` feature | `BackendUnavailable` | No — boot fails closed until engine wired |

Bounded fault cuts via `run_contained_browser_fault_matrix`: `BootStop`, `PreDispatchStop`, `LostAckUncertain`, `RestartNoReplay`. Fence-first Stop + Uncertain invariants match the synthetic harness (#543).

## Sep 18 checklist runner + evidence-pack verifier (Packet 5)

The **One-Mac / CI checklist runner** seals a JSON evidence pack without enabling
admission or claiming physical PASS. The **independent verifier** reads only the
pack file — no live backend, no runner aggregates.

### Day-30 Surface Alpha evidence ladder

| Rung | Artifact | Verifier accepts physical PASS? |
|---|---|---|
| Linux CI / default | CB dry-run pack (`ContainedBrowser`, `physical_pass_claimed: false`) | No |
| VF rehearsal | VF dry-run pack (`physical_pass_claimed: false`) | No |
| Synthetic fault matrix | Subset sealed packs with explicit fault case | No |
| Sep 18 Mac worker (future) | VF physical pack with `PhysicalProofMarkers` + live sentinel collection | Only when markers qualify |

### CLI

```sh
# Default: Contained Browser dry-run checklist → sealed pack
cargo run --locked --manifest-path crates/codegen/grokptah-isolated-surface/Cargo.toml \
  --bin grokptah-sep18-checklist -- run --output sep18-evidence-pack.json

# Independent verification (no backend)
cargo run --locked --manifest-path crates/codegen/grokptah-isolated-surface/Cargo.toml \
  --bin grokptah-sep18-checklist -- verify sep18-evidence-pack.json

# Optional VF dry-run when features allow
cargo run --locked --manifest-path crates/codegen/grokptah-isolated-surface/Cargo.toml \
  --bin grokptah-sep18-checklist -- run --vf-dry-run --output vf-dry-run-pack.json

# Bounded fault-matrix subset on CB substrate
cargo run --locked --manifest-path crates/codegen/grokptah-isolated-surface/Cargo.toml \
  --bin grokptah-sep18-checklist -- run --fault-matrix lost_ack_uncertain -o fault-pack.json
```

### Sealed pack fields (honest defaults)

- `evidence_class`: fixed at backend creation (`ContainedBrowser` default on Linux CI)
- `physical_pass_claimed`: **false** unless a future physical gate flips it with Mac markers
- `admission_available`: **false** (verifier rejects `true`)
- `host_sentinel_probes`: probe counts + channel teardown summary from Stop evidence
- `sealed_evidence`: checklist steps + Stop/Uncertain disposition when checklist completes

### Verifier reject codes (subset)

| Code | Trigger |
|---|---|
| `admission_must_stay_false` | `admission_available == true` |
| `vf_dry_run_cannot_qualify_physical_pass` / `vf_pass_claim_on_dry_run` / `isolation_pass_claim_on_dry_run` | PASS or isolation claim on any current dry-run / synthetic substrate |
| `stop_channels_still_open` | `channels_open_after_stop > 0` or `StopDestroyed` with `channels_destroyed == 0` |
| `uncertain_downgraded_after_possible_inject` | Guest-local inject without postcondition but `Stopped` disposition; LostAck/Restart fault with non-Uncertain disposition |
| `fault_matrix_disposition_mismatch` | Probe-count fingerprint mismatch (e.g. probes=4 without LostAckUncertain+Uncertain); fault-matrix encoding inconsistent with checklist |
| `checklist_incomplete` | Missing `Booted`/`StopDestroyed` when sealed; `fault_matrix_case: None` not exact happy-path; checklist steps out of canonical order or wrong exact set |
| `evidence_class_tampered` | Pack label ≠ sealed label or inconsistent with substrate |
| `host_sentinel_probe_summary_mismatch` | Pack probe summary ≠ sealed `stop_evidence` |
| `substrate_nested_evidence_missing` | CB pack sealed without nested `contained_browser.sealed_evidence` |
| `pack_sealed_evidence_mismatch` | Pack vs nested CB sealed copies differ |

Maps to Astra Sep 18 one-Mac checklist steps 1–11: runner exercises steps 2–10 on
simulator substrate; verifier is the independent gate for sealed artifacts before
any admission enablement discussion.

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
`ContainedBrowserBackend` substrate v0 (simulator rehearsal today; isolation PASS still open).

### Gate verdict

- **PASS (Sep 18):** real Mac completes the checklist with `ProofEvidenceClass::VirtualizationFramework`
  and unchanged host sentinels.
- **DRY-RUN (this slice):** `run_vf_dry_run` — honest nonclaim, `physical_pass_claimed: false`.
- **MISS → Contained Browser:** `run_contained_browser_dry_run` — honest nonclaim, `isolation_pass_claimed: false`.

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
  --test contained_browser_regression -- --test-threads=1

cargo test --locked --manifest-path crates/codegen/grokptah-isolated-surface/Cargo.toml \
  --test proof_sequencer -- --test-threads=1

cargo test --locked --manifest-path crates/codegen/grokptah-isolated-surface/Cargo.toml \
  --test proof_sequencer sep18_contained_browser -- --test-threads=1

cargo test --locked --manifest-path crates/codegen/grokptah-isolated-surface/Cargo.toml \
  --test evidence_pack_verifier -- --test-threads=1

# Checklist runner CLI smoke (CB default)
cargo run --locked --manifest-path crates/codegen/grokptah-isolated-surface/Cargo.toml \
  --bin grokptah-sep18-checklist -- run -o /tmp/sep18-evidence-pack.json

# Optional browser-engine feature (default-off; must compile fail-closed)
cargo check --locked --manifest-path crates/codegen/grokptah-isolated-surface/Cargo.toml \
  --features browser-engine

cargo test --locked --manifest-path crates/codegen/grokptah-isolated-surface/Cargo.toml \
  --features browser-engine --test browser_engine_feature -- --test-threads=1

# Mac VF dry-run rehearsal (physical worker only)
cargo test --locked --manifest-path crates/codegen/grokptah-isolated-surface/Cargo.toml \
  --features vf-backend --test proof_sequencer sep18_vf_dry_run -- --test-threads=1
```

## Residuals (honest, post-packet-7)

- `IsolatedSurfaceBackend::stop_fence_first` has no trait default — production adapters must wire real fence ack/failure.

- Checklist runner seals dry-run packs only — no live Mac VF IPC or packaged helper.
- Independent verifier is pack-only; physical Mac worker still required for VF PASS rung.
- Contained Browser substrate v0 uses an in-process simulator — not a real isolated browser engine.
- Optional `browser-engine` feature fails closed until native engine wiring lands.
- No TCC entitlement or notarization claims.
- No Windows/Linux isolated surface.
- No agent-owned cursor / surface-event stream (#286 UI layer still disposition-only).
- No bridge admission enablement — `isolated_surface_admission_available()` stays false.
- Linux CI proves contract + substrate rehearsal; physical isolation PASS is a separate exact-head gate.
- #288 packaged-VM acceptance stays open.

## Non-claims

- Simulator / Linux CI does **not** qualify a packaged VM.
- VF dry-run does **not** claim Sep 18 physical PASS.
- Synthetic harness success does **not** enable isolated visual Computer Use in production.
- `ContainedBrowser` substrate v0 does **not** prove browser isolation — only exercises the SPI lifecycle on a simulator.
