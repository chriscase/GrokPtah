# Computer Use substrate ancestry — measured DAG and residual

Produced in an isolated Cloud checkout at
`8ad3be07eb27087acb67704fdf463ecb95b64505` (tree `d3e1a6c8d209`), pinned
toolchain `1.92.0`. Nothing here is a hardware result, and nothing here is a
certification claim.

## 1. Verified refs (fail-closed identity check)

| Ref | Resolved |
| --- | --- |
| this checkout (`HEAD`) | `8ad3be07eb27087acb67704fdf463ecb95b64505` |
| `origin/main` | `67e29bd34dc64049432c715c93c2cef2185c63ea` |
| `origin/claude/grokptah-packaged-qualification-vqk7rd` | `698e445e2fec3eed31f4be8d5ed49e2782fbb895` |
| `origin/claude/grokptah-isolated-guest-reconcile-fn6j8c` (PR #439 head) | `2d4cb1728f70a1981e0583a0d4b71ebb9958b0b3` |
| `origin/codex/cu-packaged-security-hardening-v1` (PR #439 base) | `404ea3c2c46b597f2e892bb70a84b4bd25c03cbc` |
| `origin/codex/cu-isolated-guest-bootstrap-v1` | `5919e3343af20a78e17459b8ac8454bbc5aeca7e` |

## 2. Ancestry

| Ref | ahead of `main` | behind `main` | merge-base with `main` |
| --- | --- | --- | --- |
| `8ad3be07` (this checkout) | 127 | **67** | `127ffaff78b2` |
| `grokptah-packaged-qualification-vqk7rd` | 147 | **67** | `127ffaff78b2` |
| `claude/grokptah-isolated-guest-reconcile-fn6j8c` | 429 | 0 | `67e29bd34dc6` (= `main`) |
| `codex/cu-packaged-security-hardening-v1` | 422 | 0 | `67e29bd34dc6` (= `main`) |
| `codex/cu-isolated-guest-bootstrap-v1` | 348 | 0 | `67e29bd34dc6` (= `main`) |

Checked with `git merge-base --is-ancestor`:

* `8ad3be07` **is** on `grokptah-packaged-qualification-vqk7rd`, 20 commits
  before that branch's head.
* `8ad3be07` is **not** an ancestor of the PR #439 head — that lane
  deliberately excluded this one.
* The two codex refs are one linear stack: their pairwise merge-base is exactly
  `5919e334`, so **0** commits are unique to the bootstrap branch.

This reproduces the topology in PR #439's own
`docs/evidence/CU_ISOLATED_GUEST_RECONCILIATION_TOPOLOGY.md` exactly.

## 3. Complementary module sets — `src/computer_use/`

Measured by tree listing, not inference.

| Set | Modules |
| --- | --- |
| **Shared** (12) | `macos_native.rs`, `macos_native_shim.m`, `macos_observation.rs`, `mod.rs`, `platform.rs`, `policy.rs`, `projection.rs`, `reads.rs`, `service.rs`, `simulator.rs`, `store.rs`, `types.rs` |
| **Gate-native only** (this checkout) | `control.rs` — plus the `tests/mcp_computer_mutations.rs` gate |
| **PR #439 only** (17) | `coordination.rs`, `isolated_guest.rs`, `isolated_visual.rs`, `isolated_visual_artifacts.rs`, `isolated_visual_channel.rs`, `isolated_visual_driver.rs`, `isolated_visual_frames.rs`, `isolated_visual_helper.rs`, `isolated_visual_helper_control.rs`, `isolated_visual_input.rs`, `isolated_visual_input_wire.rs`, `isolated_visual_launch.rs`, `isolated_visual_protocol.rs`, `isolated_visual_runtime.rs`, `isolated_visual_stream.rs`, `macos_isolated_artifacts.rs`, `macos_isolated_runtime.rs` — plus `tests/computer_use_isolated_launch_boundary.rs` |

`8ad3be07` carries **none** of the isolated-visual substrate. It arrives in the
20 commits between here and `698e445e`. PR #439's candidate matrix is measured
against that branch head, not against this commit.

## 4. Why no code-level successor is implemented here

`control.rs` and `coordination.rs` are **not the same module renamed**. They are
different concerns, and each is load-bearing on its own lane:

| | this checkout — `control.rs` | PR #439 — `coordination.rs` |
| --- | --- | --- |
| surface | `ComputerClientIdentity`, `ComputerGrantRequest`, `ComputerAgentObservation`, `ComputerRunController`, `ComputerRunAgentController` | `ComputerSurfaceLease`, `ComputerSurfaceLeaseState`, `ComputerDispatchRecord`, `ComputerDispatchState`, `HostSurfaceLeaseRequest`, `HostLeasePriority` |
| model | actor identity, grant validation, run control | surface lease, dispatch |
| consumers here | `lib.rs`, `mcp_control.rs`, `host.rs`, `orchestration/service.rs` | — |

All five of this lane's symbols are **absent** from the PR #439 head, and
`tests/mcp_computer_mutations.rs` is absent there too. PR #439's own matrix
already grades `control.rs` as **"high — conflicts with donor design"**.

So each available code-level move is barred by the constraints on this repair:

* porting `coordination.rs` while `control.rs` stays wired into four live call
  sites would stand up a **second identity/supervision system** for one
  subsystem;
* deleting `control.rs` to adopt the PR #439 authority would break those four
  call sites and drop this lane's MCP mutation gate — **weakening gates**;
* taking the 17 PR #439 modules is a **wholesale fork merge**, and would also
  drag this shared substrate backwards (PR #439 measured the qualification lane
  as `service.rs` −4606, `store.rs` −2347, `types.rs` −2223 against its donor).

**No safe minimal ancestry correction exists at this commit.** The reconciliation
is a branch-level decision that belongs to whichever lane lands first, on a tree
that is not 67 commits behind `main`.

## 5. What is left instead

`crates/codegen/grokptah-agent-bridge/tests/computer_use_substrate_ancestry.rs`
— an assembly guard that reads sources rather than compiling them, so it holds
on every host including the macOS-only substrate a Linux `--all-targets` build
never sees. Five invariants:

1. exactly one run-control authority is assembled (`control.rs` XOR `coordination.rs`);
2. the gate-native control surface has a single definition site;
3. the isolated-visual substrate may only be assembled over `coordination.rs`;
4. every module file is declared and every declaration resolves — a
   copied-but-unwired port fails here, not at a later host-specific build;
5. the MCP mutation gate survives while `control.rs` is the authority.

Each was falsified by perturbing the tree and observing the matching test fail,
then restored. The guard is not vacuous.

## 6. Residual risk

* **The reconciliation itself is unperformed.** This guard makes a mixed
  assembly fail loudly; it does not merge the lanes. That work still needs a
  branch based on current `main`.
* The guard reasons over **file and symbol names**, not semantics. A rename that
  preserves the hazard under different names would pass it.
* Invariant 3 encodes the measured fact that PR #439's isolated-visual substrate
  is bound to `coordination.rs`. If a future donor decouples them, invariant 3
  becomes too strict and must be revised deliberately.
* Nothing here touches macOS hardware, provider behavior, VMs, or credentials.
