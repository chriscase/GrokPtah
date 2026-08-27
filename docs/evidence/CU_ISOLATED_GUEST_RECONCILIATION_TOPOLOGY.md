# Isolated-guest Computer Use — source reconciliation topology

Produced in an isolated Cloud checkout. Nothing here is a hardware result.

## 1. Verified refs (fail-closed identity check)

| Ref | Expected | Actual | Match |
| --- | --- | --- | --- |
| `origin/codex/cu-isolated-guest-bootstrap-v1` | `5919e3343af20a78e17459b8ac8454bbc5aeca7e` | `5919e3343af20a78e17459b8ac8454bbc5aeca7e` | exact |
| `origin/codex/cu-packaged-security-hardening-v1` | `404ea3c2` | `404ea3c2c46b597f2e892bb70a84b4bd25c03cbc` | exact prefix |
| `origin/claude/grokptah-packaged-qualification-vqk7rd` | `698e445e` | `698e445e2fec3eed31f4be8d5ed49e2782fbb895` | exact prefix |

`origin/main` at reconciliation time: `67e29bd34dc64049432c715c93c2cef2185c63ea`.

## 2. Ancestry

| Branch | ahead of `main` | behind `main` | merge-base with `main` |
| --- | --- | --- | --- |
| `cu-isolated-guest-bootstrap-v1` | 348 | 0 | `67e29bd34dc6` (= `main`) |
| `cu-packaged-security-hardening-v1` | 422 | 0 | `67e29bd34dc6` (= `main`) |
| `grokptah-packaged-qualification-vqk7rd` | 147 | **67** | `127ffaff78b2` |

Containment, checked with `git merge-base --is-ancestor`:

* `main` **is** an ancestor of `cu-packaged-security-hardening-v1`.
* `cu-isolated-guest-bootstrap-v1` **is** an ancestor of `cu-packaged-security-hardening-v1`
  (pairwise merge-base is exactly `5919e334`; **0** commits are unique to the bootstrap branch).
* `grokptah-packaged-qualification-vqk7rd` is **not** an ancestor of either codex branch.

So the two codex refs are one linear stack, not two histories. The hardening
branch strictly supersedes the bootstrap branch and fast-forwards from current
`main`. The qualification branch is a genuine fork.

## 3. What is already proven, and where

Measured on `cu-packaged-security-hardening-v1` (`404ea3c2`).

| Concern | Status | Home |
| --- | --- | --- |
| Guest lifecycle (`Prepared`…`Terminated`), no resume transition | proven | `isolated_visual.rs` |
| Manifest schema/backend/digest/profile/limit validation | proven | `isolated_visual.rs` |
| Cleanup evidence, crate-private constructor, surface-bound | proven | `isolated_visual.rs` |
| One-agent-per-guest lease, stale-lease denial, cancel/fail revoke | proven | `isolated_guest.rs` |
| Capture redaction (forbidden keys + host/credential/network needles) | proven | `isolated_guest.rs` |
| Artifact content measurement, read-only handles, mode/ceiling gates | proven | `isolated_visual_artifacts.rs` |
| Packaged artifact receipt bound to manifest digests | proven | `isolated_visual_artifacts.rs` |
| Helper ABI, channel binding, frame carrier, input gate | proven | `isolated_visual_{helper,channel,frames,input*}.rs` |
| Authenticated protocol envelope, size and freshness caps | proven | `isolated_visual_protocol.rs` |
| Signed-helper / entitlement discovery | macOS-only, not compiled off macOS | `macos_isolated_artifacts.rs` |
| Virtualization.framework spawn and channel handoff | macOS-only, not compiled off macOS | `macos_isolated_runtime.rs` |

### Duplicated / divergent before this change

* `descriptors_are_distinct` existed **twice**, once in `macos_isolated_runtime.rs`
  and once in `macos_isolated_artifacts.rs`. Both were inside
  `#[cfg(target_os = "macos")]`, so no non-macOS build compiled or tested either.
* Both copies accepted descriptors `0`, `1`, and `2`. A native result carrying a
  standard stream would have been admitted as a private guest channel.
* No launch-descriptor completeness type, no per-operation packaged authority,
  and no launch or cleanup receipt existed at all.

## 4. Donor selection

**Selected donor: `codex/cu-packaged-security-hardening-v1` @ `404ea3c2`** — the
smallest coherent donor, because it already contains `cu-isolated-guest-bootstrap-v1`
whole and fast-forwards from current `main`, so selecting it mixes no histories.

**`grokptah-packaged-qualification-vqk7rd` is deliberately not mixed in.** It
forked at `127ffaff`, is 67 commits behind `main`, and its shared substrate is
thousands of lines older than the donor's:

| File | qualification vs donor |
| --- | --- |
| `service.rs` | +817 / −4606 |
| `store.rs` | +72 / −2347 |
| `types.rs` | +90 / −2223 |
| `policy.rs` | +64 / −776 |
| `projection.rs` | +22 / −636 |
| `macos_observation.rs` | +220 / −1631 |

Taking its isolated-visual modules would drag that substrate backwards.

### Candidate matrix — qualification-only modules, for later reconciliation

These are catalogued, **not** ported. Each would need re-basing onto the donor
substrate on its own branch.

| Module | Bytes | What it adds | Port risk |
| --- | --- | --- | --- |
| `isolated_visual_gates.rs` | 17.3k | a11y/privacy/security/cross-boundary gates; reads the shim via `include_str!` so Linux CI watches it | low — test-only, mostly substrate-independent |
| `isolated_visual_cleanup_gates.rs` | 10.5k | terminal cleanup may not claim more than the host observed | low–medium |
| `isolated_visual_leak_gates.rs` | 21.2k | orphan descriptor/socket/process/mount/overlay/lease leak freedom | medium — uses `UnixStream` and deadlines |
| `isolated_visual_selfcheck.rs` | 47.8k | allocation-only rehearsal keeping the substrate live under `-D warnings` | medium — broad substrate surface |
| `isolated_visual_harness.rs` | 22.5k | measured-launch policy over a `MeasuredLaunchSteps` trait, hardware-gated | medium |
| `isolated_visual_soak.rs` | 9.6k | source canary vs hardware soak, separated by `HardwareGateEvidence` | medium — depends on harness |
| `isolated_visual_status.rs` | 6.0k | read-only availability report, no runtime type crosses it | low |
| `isolated_visual_package.rs` | 8.4k | shipped package invariants vs the Rust contract | low |
| `control.rs` | 4.9k | MCP mutation adapter (donor has `coordination.rs` instead) | **high — conflicts with donor design** |

## 5. Pre-existing failures on the donor base

Reproduced on a clean checkout of `404ea3c2` with **zero** changes applied, then
again after this change with identical results. None is in a file this change
touches.

| Test | Failure |
| --- | --- |
| `computer_use::isolated_visual::tests::serialized_contract_contains_no_host_paths_or_channel_secret` | `assertion failed: !encoded.contains(forbidden)` |
| `computer_use::isolated_visual_channel::tests::canonical_binding_vector_matches_freestanding_guest` | `left: 115, right: 114` |
| `computer_use::isolated_visual_runtime::tests::runtime_requires_binding_before_channels_and_stop` | `isolated frame request nonce is not canonical` |
| `computer_use::macos_observation::tests::secure_values_are_removed_and_evidence_is_exactly_scoped` | `left: 2, right: 1` |
| `computer_use_release_gate::rust_computer_use_sources_remain_free_of_global_input_injection` | `macos_native_shim.m production source must not contain CGEventCreate` |

These are recorded, not fixed: each is outside the packaged launch boundary this
change implements.

## 6. Compiled proof vs hardware proof

**Compiled and run in Cloud (Linux, x86_64):**

* Portable launch-descriptor admission: completeness, standard-stream refusal,
  aliasing, range, implausible/self process id.
* Packaged authority: receipt↔manifest binding, exact run/surface/input-domain
  binding, one-agent lease, stale-lease refusal, per-operation gating,
  revocation, cancel race, cleanup-once.
* Deterministic launch/cleanup receipts and their leak-freedom.
* Protocol caps and misbinding refusals; artifact measurement against redirected
  symlinks, wrong modes, and oversize.
* With `--features macos-source-typecheck`: `macos_isolated_runtime.rs` and
  `macos_isolated_artifacts.rs` typecheck and their 6 unit tests run.

**Not proven anywhere in this branch — macOS hardware campaign:**

1. Signed helper: real `codesign` verification, designated requirement, hardened
   runtime, sandbox, and entitlement checks against a shipped bundle.
2. Guest boot: Virtualization.framework configuration accepted, guest reaching
   read-only ready.
3. Rendered frames: real frames crossing the private frame channel within the
   manifest's geometry and byte ceilings.
4. Host input: real input packets accepted by a booted guest, and freshness
   fencing against a live frame.
5. Cleanup: helper process actually reaped, descriptors closed, overlay and
   frame cache actually removed, verified out of band.
6. Soak: repeated launch/stop cycles with no leaked process, descriptor, mount,
   overlay, or lease.

Also unproven off macOS: `O_NOFOLLOW` refusal to follow a symlink at open time.
This branch proves only that a followed symlink's *target* is still measured and
still fails closed when hostile.
