# Isolated visual Computer Use — v13 packaged qualification handoff

Status: **queued procedure only; no VM, package, or Computer Use capability is
qualified by this document.** The active packaged lease-fence campaign is a
separate source gate at `b250b70`; do not substitute this bundle into that
running campaign or launch both campaigns concurrently.

## Frozen input

- Immutable source bundle: `/private/tmp/grokptah-cu-isolated-visual-v13.bundle`
- Bundle SHA-256: `3b3186db66477d3ab9867e1ee1a59d6071b7e0aab684b241e997cb57ae83e212`
- Source ref: `codex/cu-isolated-guest-bootstrap-v1`
- Source cutoff: `890bb080104c24dcfb0da787e7d0b20eb875f7c3`
- Source parent: `0833987cb8d876bb908704dc929ee1d87c6f9ae9`
- Developer checkout (must remain untouched):
  `6409645cb7d0fe6d75585f0610366340f808b8ec`

The bundle has complete history and was verified with `git bundle verify`. The
handoff file itself was added after the bundle was sealed; the bundle is the
only source input for the campaign.

## Copyable external prompt

```text
Run the GrokPtah Stage 9 / #288 isolated visual Computer Use qualification
campaign from the exact immutable bundle below. This is a fail-closed
qualification procedure, not an implementation task.

Bundle: /private/tmp/grokptah-cu-isolated-visual-v13.bundle
Bundle SHA-256: 3b3186db66477d3ab9867e1ee1a59d6071b7e0aab684b241e997cb57ae83e212
Source cutoff: 890bb080104c24dcfb0da787e7d0b20eb875f7c3

Create a disposable checkout from that bundle and verify its SHA, complete
history, exact HEAD, and clean worktree. Do not modify the developer checkout,
existing app sessions, branches, GitHub, or the currently running b250b70
packaged lease campaign. Do not infer a VM capability from source tests,
Linux image reproducibility, package signing, or launchAttempted=false.

Before every Rust command, set exactly:
RUSTC_WRAPPER=/opt/homebrew/bin/sccache
SCCACHE_DIR=/Users/chriscase/Library/Caches/grokptah/sccache
CARGO_TARGET_DIR=/Users/chriscase/Library/Caches/grokptah/targets/rust-1.92.0-stage5-memory-default
Reuse that target serially. Report OS/build, Virtualization.framework
availability, signing-identity fingerprint (never private material), free
disk, active cargo/rustc owners, target path, and owner before building; report
target size, lsof/open-handle state, and cleanup/retention afterward. Abort if
the identity, hardware capability, disk headroom, or exact disposable output
preconditions are absent.

Follow docs/COMPUTER_USE_ISOLATED_QUALIFICATION_RUNBOOK.md exactly:
1. Run the repository-owned guest/helper source verifiers.
2. Build the pinned guest image twice on the pinned Linux runner and compare
   image and manifest bytes; stop on any mismatch.
3. Package the reviewed image with the repository-owned signed-app script on a
   credentialed macOS host. Verify helper-only App Sandbox + Virtualization
   entitlements, no network/share/clipboard/credential/host-input/USB/
   camera/microphone devices, matching identities, exact manifest digests, and
   strict deep signing. A failed package leaves no promoted output.
4. Run the packaged supervisor from the correctly signed GrokPtah app bundle,
   not an unsigned helper. Retain secret-free evidence for Prepared -> Running
   -> Bound -> Stopping -> CleanupPending -> Terminated; frame sequence,
   dimensions, digest and freshness; pointer move/click/scroll/drag,
   keyboard navigation and Unicode text; stale/replay/wrong-challenge/
   malformed-input/duplicate-held-state rejection; helper kill, guest crash,
   VM-start failure, timeout, Stop, Take over, restart, and exact PID,
   descriptor, overlay and frame-cache cleanup.
5. Prove the host foreground app, active window, physical pointer, clipboard
   digest, and unrelated windows are unchanged before/after every interaction.
   Uncertain input poisons the surface; never retry or resume it automatically.
6. Obtain independent security and accessibility/hardware review plus the
   expert UI review on this exact packaged identity.

If any required capability or evidence is unavailable, return NOT QUALIFIED and
stop. Do not enable HostNative or VisualFallbackAct, close #288, or infer a
hardware claim. Return one dated, secret-free report with exact source/package/
guest/configuration/manifest digests, all stage results, negative cases,
cleanup ownership, reviewer roles, and an explicit QUALIFIED or NOT QUALIFIED
decision. Attach it only to source cutoff 890bb080… .
```

## Interpretation

Only a complete report covering the signed package, real VM boot, rendered
frames, host input, takeover, restart, cleanup, and independent review may
advance Stage 9/#288. A source pass, package-signature pass, or
`launchAttempted: false` remains **NOT QUALIFIED**.
