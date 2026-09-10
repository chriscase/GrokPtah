# Isolated Surface Proof Harness v0

This document inventories the current `main` Computer Use / isolated-guest code,
defines the synthetic proof harness, and maps it to the Sep 5–18 2026 calendar
(Phase-1 packet 11: Contained Browser clipboard isolation kill-gate). It does **not**
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
| Sep 6–12 | Packet 7 — `stop_fence_first` no default (#549) | **Landed** — trait default removed; production adapters must fence with ack/failure |
| Sep 6–12 | Packet 8 — Mac host sentinel provenance (#550) | **Landed** — `MacHostSentinelCollector::collect()` wired; synthetic self-compare ineligible for physical markers |
| Sep 6–12 | Packet 9 — Honest Contained Browser captured-frame hashes (#551) | **Landed** — `GuestFrame.digest` is `sha256:<64 hex>` over bounded capture bytes; simulator payloads labeled synthetic |
| Sep 18 | Packet 10 — Native host-sentinel runner (#554) | **Landed** — exclusive `MacHostSentinelCollector` runner; Linux fail-closed; no PASS/admission |
| Sep 8–17 | Packet 11 — Contained Browser clipboard isolation kill-gate | **This slice** — `ClipboardWitness` + `WKClipboardProbe` private-world protocol; Linux unsupported; never admission |
| Sep 18 | Physical Mac gate | VF PASS or honest Contained Browser pivot |

## Exact-main inventory (base `c3dbb8b5f410dad4f411a10ddcbf02dcda6660a0` + packet 11)

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
| Content-addressed captured-frame seam | `captured_frame.rs` — digest from bounded bytes only; public metadata is length/source/media/dimensions |
| Contained Browser dry-run sequencer path | `contained_browser_dry_run.rs` + `Sep18NoModelProofSequencer::run_contained_browser_dry_run` |
| Contained Browser fault matrix | `run_contained_browser_fault_matrix` — bounded fault cuts on CB substrate |
| Contained Browser regression | `tests/contained_browser_regression.rs` |
| VF backend stub (Mac + `vf-backend` feature) | `vf_backend.rs` — honest `VirtualizationFramework` label, dry-run only |
| VF dry-run sequencer path | `vf_dry_run.rs` + `Sep18NoModelProofSequencer::run_vf_dry_run` |
| Native host-sentinel runner | `native_sentinel_runner.rs` + `Sep18NoModelProofSequencer::run_native_host_sentinel` |
| Contained Browser clipboard kill-gate | `clipboard_witness.rs` + `wk_clipboard_probe.rs` + `clipboard_kill_gate.rs` |
| Sep 18 no-model proof sequencer | `proof_sequencer.rs` — checklist + bounded fault matrix |
| Harness orchestrator | `harness.rs` — backend-generic, `with_vf_backend(receipt)` only for VF label |
| Channel destroy registry | `channels.rs` |
| Restart snapshot for recovery tests | `store.rs` |
| Crash/Stop regression suite | `tests/stop_regression.rs` |
| Sequencer + SPI regression | `tests/proof_sequencer.rs` |
| Bridge fail-closed seam | `grokptah-agent-bridge/src/computer_use/isolated_surface.rs` |
| Bridge integration tests | `grokptah-agent-bridge/tests/isolated_surface_proof_harness.rs` |
| Sep 18 checklist runner | `checklist_runner.rs` — default CB dry-run, optional VF dry-run / native-sentinel / fault matrix |
| Sealed evidence pack + verifier | `evidence_pack.rs` — independent accept/reject with explicit codes |
| Checklist CLI | `src/bin/grokptah-sep18-checklist.rs` — `run` + `verify`; `--native-host-sentinels` exclusive; `--clipboard-kill-gate` exclusive |
| Evidence-pack verifier tests | `tests/evidence_pack_verifier.rs` — happy path + tamper rejection |
| Native host-sentinel runner tests | `tests/native_sentinel_runner.rs` — Linux fail-closed + forged live/fallback/PASS rejection |
| Clipboard kill-gate tests | `tests/clipboard_kill_gate.rs` — Linux unsupported + forged Pass / host drift / stale generation / duplicate-missing / forbidden eval |
| Captured-frame adversarial tests | `tests/captured_frame_evidence.rs` — digest recomputation, tamper/oversize, no raw-byte leak |

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

1. Capture baseline before launch with `MacHostSentinelCollector::collect()` (native AX/CGEvent/clipboard/checkout fence; fails closed without Accessibility trust on macOS).
2. Attach the collector with `IsolatedSurfaceHarness::attach_native_collector` **before** any probe, or run `Sep18NoModelProofSequencer::run_native_host_sentinel` / `grokptah-sep18-checklist run --native-host-sentinels`.
3. At boot, inject, and Stop probe points the attached collector is used exclusively via [`HostSentinelRegistry::refresh_from_native_collector`]. Compare-only `refresh_host_sentinels(snapshot)` is forbidden after attachment.
4. `StopEvidence.host_sentinels_unchanged` remains authoritative only when probes match baseline.
5. `StopEvidence.live_host_sentinel_collection` is **true** only after at least one successful native Mac probe — never from [`SyntheticHostProbe`] rehearsal self-compare.
6. `PhysicalProofMarkers.live_host_sentinel_collection` must stay `dry_run_none()` on this runner; live collection does not qualify VF/isolation PASS. The independent verifier rejects forged markers and any synthetic fallback flag.

**Sentinel provenance (packet 8):**

| Probe source | Rehearsal / unit tests | Physical Mac proof markers |
|---|---|---|
| [`SyntheticHostProbe`] (`synthetic-rehearsal-only-not-physical-proof`) | Yes — default harness path | **Never** |
| [`MacHostSentinelCollector::collect()`] + `refresh_from_native_collector` | macOS worker with Accessibility trust | Required for live collection marker |
| `refresh_host_sentinels(snapshot)` with pre-built snapshot | Compare-only hook | **Not** native collection |

Synthetic harness / Linux CI / dry-run packs never set `live_host_sentinel_collection: true`. #288 packaged-VM acceptance stays open.

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

### Contained Browser captured-frame hashes (packet 9)

Every Contained Browser `GuestFrame.digest` is canonical `sha256:` + 64 lowercase
hex computed from the **exact bounded capture bytes**. Digests are never built
from labels, booleans, paths, or caller-provided digest claims.

Public/sealed metadata is limited to byte length, source/media classification, and
fixed simulator dimensions. Raw frame bytes must not appear in `GuestFrame` JSON,
sealed packs, snapshots, logs, or error strings.

The default simulator emits deterministic explicit synthetic payload bytes for CI,
labeled `synthetic_simulator` / `synthetic_payload`. That is **not** a real browser
capture and does **not** change `CONTAINED_BROWSER_DRY_RUN_NONCLAIM`.

The optional `browser-engine` feature stays fail-closed: this slice does not wire an
engine capture, fabricate an engine receipt, emit native/physical markers, or claim
PASS. Admission and Computer Mode stay false.

Empty, oversized, malformed, caller-digest, one-byte-tampered, stale/misbound, and
source-upgraded frames are rejected before postcondition evidence can be sealed.
Epoch still increments on inject; digest change tracks captured-byte change.

Bridge admission `isolated_surface_admission_available()` remains **false**.

### Contained Browser clipboard isolation kill-gate (packet 11 / Sep 8–17)

Phase-1 slice proving whether page-local copy/cut/paste/write can be mediated
without reading or mutating the host `NSPasteboard`. This is **not** VF PASS,
isolation PASS, Computer Mode, or admission.

| Component | Role |
|---|---|
| `ClipboardWitness` | Seals before/after host clipboard **digests** from change-count + type names only. Never reads pasteboard data, never writes the general pasteboard, never uses CGEvent, Accessibility, or AppleScript. |
| `WKClipboardProbe` | Private `WKContentWorld` (`grokptah.clipboard.probe.v1`) pull/reply. The contained **page world** is never targeted with `evaluateJavaScript` or `callAsyncJavaScript`. |
| `run_clipboard_kill_gate` | Orchestrates witness + probe; seals typed evidence with verdict `pass` / `fail` / `inconclusive` / `unsupported`. |

**Verdict contract (fail closed):**

| Verdict | Meaning | Who may seal it |
|---|---|---|
| `unsupported` | Non-macOS / no WebKit | Linux CI (deterministic) |
| `inconclusive` | Timeout, navigation/epoch drift, malformed/duplicate/missing reply, missing WebKit, permission prompt, uncertain | Any platform; never Pass |
| `fail` | Host clipboard digest changed (isolation broken) | Native Mac probe only |
| `pass` | Native Mac WebKit private-world pull, four mediated receipts, host digest unchanged | **Only** exact-head native Mac worker; Linux verifiers reject Pass |

Any timeout, navigation/epoch drift, malformed/duplicate reply, unexpected host
clipboard change, unsupported platform, missing WebKit capability, permission
prompt, or uncertain result **fails closed and never claims Pass**.

Forbidden on this slice: host CGEvent, Accessibility input, AppleScript, global
pasteboard writes, browser auth, provider calls, private data, shared dirs,
network. Concurrent Computer Use, Windows/Linux adapters, and the public SDK
are untouched.

#### Physical Mac instructions (exact HEAD)

Run on a macOS worker at the **exact** branch HEAD. Linux CI success is **not**
physical Pass. Do not enable admission.

```sh
# 1. Confirm HEAD (replace with the merged/review SHA you are proving)
git rev-parse HEAD

# 2. Run the exclusive clipboard kill-gate (no --vf-dry-run, no --native-host-sentinels)
cargo run --locked --manifest-path crates/codegen/grokptah-isolated-surface/Cargo.toml \
  --bin grokptah-sep18-checklist -- run --clipboard-kill-gate \
  -o clipboard-kill-gate.json

# 3. Independent verify on the same Mac (Linux verifiers reject Pass by contract)
cargo run --locked --manifest-path crates/codegen/grokptah-isolated-surface/Cargo.toml \
  --bin grokptah-sep18-checklist -- verify clipboard-kill-gate.json
```

Inspect `clipboardKillGate.verdict`:

- `pass` — native private-world mediation with unchanged host digest. Still
  `physicalPassClaimed=false`, `isolationPassClaimed=false`, `vfPassClaimed=false`,
  `admissionAvailable=false`.
- `fail` — host digest changed; page-local mediation did not hold.
- `inconclusive` — timeout / WebKit / protocol / permission; not Pass.
- `unsupported` — this worker cannot run WebKit (should not happen on macOS;
  if it does, treat as fail-closed, not Pass).

**Nonclaims for this physical run:**

- Kill-gate Pass is **not** Sep 18 VF PASS, packaged-VM qualification, or
  Computer Mode.
- Synthetic / Linux CI tests prove verifier rejection of forged Pass; they
  **cannot** establish physical Mac Pass.
- Do not merge this as admission enablement. Do not close #288/#286 on this
  slice alone.

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
| Native host-sentinel runner | Native pack (`Synthetic` guest, `PhysicalProofMarkers::dry_run_none()`) | No |
| Clipboard kill-gate | Clipboard pack (`ContainedBrowser`, kill-gate verdict independent of VF PASS) | No — kill-gate Pass ≠ physical VF PASS; Linux verifiers reject Pass |
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

# Native host-sentinel runner (Linux: honest UnsupportedPlatform pack; macOS uses live collector)
cargo run --locked --manifest-path crates/codegen/grokptah-isolated-surface/Cargo.toml \
  --bin grokptah-sep18-checklist -- run --native-host-sentinels --vf-dry-run \
  --checkout /absolute/path/to/disposable/checkout -o native-sentinel-pack.json

# Contained Browser clipboard isolation kill-gate (Linux: honest Unsupported; macOS: native WK probe)
cargo run --locked --manifest-path crates/codegen/grokptah-isolated-surface/Cargo.toml \
  --bin grokptah-sep18-checklist -- run --clipboard-kill-gate -o clipboard-kill-gate.json
```

### Sealed pack fields (honest defaults)

- `evidence_class`: fixed at backend creation (`ContainedBrowser` default on Linux CI)
- `physical_pass_claimed`: **false** unless a future physical gate flips it with Mac markers
- `admission_available`: **false** (verifier rejects `true`)
- `host_sentinel_probes`: probe counts + channel teardown summary from Stop evidence
- `sealed_evidence`: checklist steps + Stop/Uncertain disposition when checklist completes
- `contained_browser.captured_frames`: public before/after hashes (byte length, source, dimensions) — never raw bytes

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
| `captured_frame_evidence_invalid` | Missing, non-canonical, source-upgraded, or non-changing sealed captured-frame metadata |
| `clipboard_kill_gate_synthetic_cannot_pass` | Synthetic fixture sealed kill-gate Pass |
| `clipboard_kill_gate_pass_on_unsupported_platform` | Pass/mediation on non-macOS pack or Linux verifier |
| `clipboard_kill_gate_host_digest_drift` | Before/after host clipboard digests disagree (or contents were read/written) |
| `clipboard_kill_gate_forbidden_script_evaluation` | Page-world `evaluateJavaScript` / `callAsyncJavaScript` |
| `clipboard_kill_gate_stale_generation` / `duplicate_reply` / `malformed_reply` / `missing_reply` | Protocol fail-closed |
| `clipboard_kill_gate_uncertain_cannot_pass` | Uncertain/incomplete native contract claimed Pass |
| `clipboard_kill_gate_cannot_qualify_physical_pass` | Forged VF/physical markers on the clipboard kill-gate substrate |

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
| 1 | Capture host baseline | `MacHostSentinelCollector::collect()` → store snapshot (native; fails closed without Accessibility trust) |
| 2 | Arm harness | `IsolatedSurfaceHarness::with_vf_backend(baseline, backend, receipt)` |
| 3 | Launch + boot guest | `boot()` → `Ready`, first frame epoch > 0 |
| 4 | Probe sentinels | `refresh_host_sentinels_from_collector(collector)` — no drift; native provenance recorded |
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

cargo test --locked --manifest-path crates/codegen/grokptah-isolated-surface/Cargo.toml \
  --test captured_frame_evidence -- --test-threads=1

cargo test --locked --manifest-path crates/codegen/grokptah-isolated-surface/Cargo.toml \
  --test native_sentinel_runner -- --test-threads=1

cargo test --locked --manifest-path crates/codegen/grokptah-isolated-surface/Cargo.toml \
  --test clipboard_kill_gate -- --test-threads=1

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

## Residuals (honest, post-packet-11)

- `IsolatedSurfaceBackend::stop_fence_first` has no trait default — production adapters must wire real fence ack/failure.
- `MacHostSentinelCollector` requires macOS Accessibility trust for foreground/unrelated window evidence; missing TCC → honest `BackendUnavailable`, not synthetic PASS.
- Default harness rehearsal still uses [`SyntheticHostProbe`]. Native mode requires
  `--native-host-sentinels`, `--vf-dry-run`, and an explicit disposable `--checkout PATH`;
  it attaches the collector before VF boot/lifecycle/Stop and never falls back to synthetic probes.

- Checklist runner seals dry-run / native-sentinel packs only — no live Mac VF IPC or packaged helper.
- Independent verifier is pack-only; physical Mac worker still required for VF PASS rung.
- Contained Browser substrate v0 uses an in-process simulator — not a real isolated browser engine.
- Simulator captured-frame bytes are explicit synthetic payloads, content-addressed and labeled synthetic — not a real browser capture.
- Optional `browser-engine` feature fails closed until a bounded engine capture is actually wired; this slice fabricates no engine receipt or PASS.
- Native host-sentinel live collection is **not** VF PASS, isolation PASS, or admission enablement.
- Clipboard kill-gate Pass is **not** VF PASS, isolation PASS, Computer Mode, or admission. Linux CI is deterministic `unsupported`. Only exact-head native Mac WebKit evidence may seal kill-gate Pass, and Linux verifiers reject Pass.
- No TCC entitlement or notarization claims.
- No Windows/Linux isolated surface.
- No agent-owned cursor / surface-event stream (#286 UI layer still disposition-only).
- No bridge admission enablement — `isolated_surface_admission_available()` stays false.
- Linux CI proves contract + substrate rehearsal; physical isolation PASS is a separate exact-head gate.
- #288 packaged-VM acceptance stays open. #286 stays open.

## Non-claims

- Simulator / Linux CI does **not** qualify a packaged VM.
- VF dry-run does **not** claim Sep 18 physical PASS.
- Synthetic harness success does **not** enable isolated visual Computer Use in production.
- [`SyntheticHostProbe`] self-compare does **not** qualify as physical Mac host sentinel collection.
- `ContainedBrowser` substrate v0 does **not** prove browser isolation — only exercises the SPI lifecycle on a simulator.
- Simulator captured-frame hashes are **not** a real browser capture; they content-address labeled synthetic payload bytes.
- Native host-sentinel runner success is **not** VF/isolation/physical PASS and does not enable Computer Mode or admission.
- Clipboard kill-gate success is **not** VF/isolation/physical PASS. Synthetic verifier fixtures cannot establish physical Mac Pass. Linux CI `unsupported` is not Fail of isolation and not Pass.
