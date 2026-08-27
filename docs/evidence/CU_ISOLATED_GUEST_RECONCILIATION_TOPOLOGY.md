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

## 5. Donor-base failures: repaired here, and what remains

All of these were reproduced on a clean checkout of `404ea3c2` with **zero**
changes applied before being touched. None originates in the packaged launch
boundary this branch adds.

### Repaired (five gates that were failing, none weakened)

| Test | Root cause | Repair |
| --- | --- | --- |
| `isolated_visual::…::serialized_contract_contains_no_host_paths_or_channel_secret` | the needle `"credential"` matched `credentialForwarding`, the profile field that *proves* forwarding is off | ban the value-bearing names instead, and assert `credentialForwarding:false`, `hostClipboard:false`, `sharedDirectories:false` — stronger than the absence of a word |
| `isolated_visual_channel::…::canonical_binding_vector_matches_freestanding_guest` | test arithmetic: `domain-1` is 8 bytes, the expectation said 7 | corrected to 8; the exact packet length is still pinned |
| `isolated_visual_runtime::…::runtime_requires_binding_before_channels_and_stop` | fixture used a bare 32-hex request nonce; the protocol requires a canonical UUIDv4 | fixture uses a real UUIDv4; the product gate is untouched |
| `macos_observation::…::secure_values_are_removed_and_evidence_is_exactly_scoped` | the fixture gained a third node, so the surviving count moved 1 → 2. The product was already dropping the secure node correctly | assert the surviving *set* and that no surviving role contains `secure`, plus that neither the secret value, its label, nor its role escapes |
| `computer_use_release_gate::rust_computer_use_sources_remain_free_of_global_input_injection` | the shim built a `CGEvent` purely to read the pointer for the before/after interaction fence, pulling in the banned global-injection family | read the pointer with AppKit `NSEvent.mouseLocation`. Only equality between two samples is ever used, so the differing coordinate origin does not change the fence. The gate still forbids `CGEventCreate` |

### Still failing on the donor base, outside this lane

| Test | Failure | Why it is not repaired here |
| --- | --- | --- |
| `mcp_continuity_probe::continuity_probe_is_evidence_first_and_recoverable` | `continuity probe harness timed out` after 98s | Reproduced identically on clean `404ea3c2`. Installing the harness's npm dependencies does not change it. MCP continuity lane, not packaged Computer Use |
| 6 × `grokptah-service` `always_on_grokbot` tests | all six panic with `unknown tool ptah_set_managed_execution` | The tool exists in the bridge (`mcp_control.rs`) but the hosted service does not expose it. That is an always-on lane gap; no service production code is touched here |

## 5a. CI status

Three checks were red on the pull request. Each was diagnosed to its first
failing step.

| Check | Host | First failing step | Status |
| --- | --- | --- | --- |
| `always-on-grokbot` | ubuntu | `cargo clippy --locked --all-targets -- -D warnings` in `grokptah-service` — 9 lints in test sources | **repaired**, exact CI command now passes |
| `hosted-service` | ubuntu | same service clippy step | **repaired**, same fix |
| `desktop` | macOS | `grokptah-desktop` failed to compile: `unresolved import grokptah_agent_bridge::ComputerSurfaceCoordination` | **repaired** — the type was exported from `computer_use` but never re-exported at the crate root |

Two more macOS-only failures surfaced behind that one, both the same shape:
`pub(crate) use macos_isolated_runtime::IsolatedVisualPackagedRuntime` rejected
as an unused import, and `examples/macos_computer_use_background_text.rs`
importing `SemanticElement` from the crate root. Every one of these is
invisible to a non-macOS `--all-targets` build, and each cost a full macOS CI
round to find.

`tests/crate_root_exports.rs` closes that class: it reads every target's
`grokptah_agent_bridge::Name` imports — including the nested desktop crate's —
and asserts each resolves against `lib.rs`. It reads sources rather than
compiling them, so it holds on every host. Removing `SemanticElement` from
`lib.rs` makes it fail naming that exact file and symbol, so it is not a
vacuous gate.

The service lint repairs keep every assertion. The one that guards a gate,
`assert!(PRELOAD_IMMUTABLE_GOLDEN, …)`, became `const { assert!(…) }`: the same
message, now checked at compile time, so a build that flipped the flag cannot
produce the test binary at all.

Beyond its first failure the `desktop` job also runs bridge clippy under
`-D warnings` on macOS. Two further classes were repaired for it: four
platform-independent lints (`manual_is_multiple_of`, three `too_many_arguments`,
and an `await_holding_lock`), and the dead-code rejection of the deliberately
undispatched packaged supervisor, which is now documented with a single
module-level allow naming the change that must remove it.

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
* Public wire contract of the launch vocabulary: channel roles, guest
  operations, authority states, and revocation reasons all pin their exact
  serialized form, because receipts embed those names.
* With `--features macos-source-typecheck`: `macos_isolated_runtime.rs` and
  `macos_isolated_artifacts.rs` typecheck and their 6 unit tests run.

**Not verifiable in this container — exact host blockers:**

* **macOS steps of the `desktop` job.** There is no macOS host and no macOS
  SDK here. `cargo check --target aarch64-apple-darwin` fails in `ring`'s C
  build before reaching any GrokPtah source. The `macos-source-typecheck`
  feature is the substitute and is deliberately narrower: it compiles the two
  isolated modules **and the crate-internal re-export of the packaged
  supervisor**. That last line matters — it is `#[cfg(target_os = "macos")]`,
  so while the feature did not cover it, a `-D warnings` `unused_imports`
  error on it was invisible off macOS and only surfaced in CI. Widening it to `macos_native` was tried and reverted —
  that module's live roots are macOS-`cfg` call sites, so on Linux the whole
  module reads as dead and the lint surface loses fidelity rather than gaining
  it. `native_context` is the one known false positive: it is live on macOS via
  `macos_native.rs:328`.
* **The Objective-C shim edit.** Replacing `CGEventCreate` with
  `NSEvent.mouseLocation` is verified by the release gate (which reads the
  source text) and by review. It is **not** compile-verified: `build.rs` skips
  the shim off macOS, and no macOS SDK is available here.

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
