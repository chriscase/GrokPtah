# Isolated visual Computer Use — v15 packaged qualification handoff

Status: **queued procedure only; no VM, package, or Computer Use capability is
qualified by this document.** v15 supersedes v14 by including the regression
that proves a poisoned input gate cannot leave the lifecycle live after a stop
failure; the supervisor aborts and requires cleanup.

The earlier `b250b70` source campaign is complete; do not substitute its
source-only report or bundle for this packaged/hardware campaign.

## Frozen input

- Immutable source bundle: `/private/tmp/grokptah-cu-isolated-visual-v15.bundle`
- Bundle SHA-256: `34ecdcdacf6c07b07d425e56c0f908ba8f6a5932d75f0dd2abb88c5c30bb8012`
- Source ref: `codex/cu-isolated-guest-bootstrap-v1`
- Source cutoff: `2142287f67fe532a631d72f280c91bb8eae38b22`
- Parent: `0bcb8430ce3eb62400b7d599f2c89774afe11030`
- Developer checkout (must remain untouched):
  `6409645cb7d0fe6d75585f0610366340f808b8ec`

The bundle has complete history and passes `git bundle verify`. This handoff
file is documentation added after sealing; the bundle is the only source input.

## Copyable external prompt

```text
Run the GrokPtah Stage 9 / #288 isolated visual Computer Use qualification
campaign from this exact immutable bundle. This is a fail-closed qualification
procedure, not an implementation task.

Bundle: /private/tmp/grokptah-cu-isolated-visual-v15.bundle
Bundle SHA-256: 34ecdcdacf6c07b07d425e56c0f908ba8f6a5932d75f0dd2abb88c5c30bb8012
Source cutoff: 2142287f67fe532a631d72f280c91bb8eae38b22

Create a disposable checkout and verify bundle SHA, complete history, exact
HEAD, and clean worktree. Keep the developer checkout, existing app sessions,
Git branches, GitHub, and the recorded b250b70 source result untouched. Do not infer
VM capability from source tests, Linux image reproducibility, package signing,
or launchAttempted=false.

Before every Rust command set exactly:
RUSTC_WRAPPER=/opt/homebrew/bin/sccache
SCCACHE_DIR=/Users/chriscase/Library/Caches/grokptah/sccache
CARGO_TARGET_DIR=/Users/chriscase/Library/Caches/grokptah/targets/rust-1.92.0-stage5-memory-default
Reuse that target serially. Report OS/build, Virtualization.framework,
non-ad-hoc signing identity fingerprint (never private material), disk,
active cargo/rustc owners, target path/owner before building, and target size,
lsof/open handles, and cleanup/retention afterward. Abort on missing host
capability or an unsafe output precondition.

Follow docs/COMPUTER_USE_ISOLATED_QUALIFICATION_RUNBOOK.md exactly:
1. Run the repository-owned guest/helper source verifiers.
2. Build the pinned guest image twice on the pinned Linux runner and compare
   image and manifest bytes.
3. Package the reviewed image on a credentialed macOS host with the
   repository-owned signed-app script. Verify helper-only App Sandbox +
   Virtualization entitlements, no network/share/clipboard/credential/
   host-input/USB/camera/microphone devices, matching identities, exact
   manifest digests, and strict deep signing. Failed packaging leaves no
   promoted output.
4. Run the signed packaged supervisor and retain secret-free evidence for
   Prepared -> Running -> Bound -> Stopping -> CleanupPending -> Terminated;
   frame sequence/dimensions/digest/freshness; bounded pointer, keyboard,
   Unicode text, Stop, Take over, restart, helper/guest failure, malformed or
   replayed input, and exact process/descriptor/overlay/frame-cache cleanup.
   The poisoned-input stop regression must be green.
5. Prove foreground app, active window, physical pointer, clipboard digest, and
   unrelated windows are unchanged before/after every interaction. Never retry
   uncertain input or resume it automatically.
6. Obtain independent security, accessibility/hardware, and expert UI review
   on this exact packaged identity.

If any item is unavailable, return NOT QUALIFIED and stop. Do not enable
HostNative or VisualFallbackAct, close #288, or infer hardware capability.
Return one dated, secret-free report with exact source/package/guest/
configuration/manifest digests, all stage results, negative cases, cleanup
ownership, reviewer roles, and an explicit QUALIFIED or NOT QUALIFIED decision.
Attach it only to source cutoff 2142287f… .
```

## Interpretation

Only a complete report covering signed packaging, real VM boot, rendered
frames, isolated host input, takeover, restart, cleanup, and independent review
may advance Stage 9/#288. A source pass, package-signature pass, or
`launchAttempted: false` remains **NOT QUALIFIED**.
