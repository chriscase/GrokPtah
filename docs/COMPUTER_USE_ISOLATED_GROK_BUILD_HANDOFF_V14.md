# Isolated visual Computer Use — v14 packaged qualification handoff

Status: **queued procedure only; no VM, package, or Computer Use capability is
qualified by this document.** This v14 bundle supersedes v13 because it also
contains the fail-closed stop-control repair: a stop failure before `Stopping`
now aborts the helper, revokes the lease, and requires exact cleanup.

Do not substitute this bundle into the currently running `b250b70` source
campaign or launch it concurrently.

## Frozen input

- Immutable source bundle: `/private/tmp/grokptah-cu-isolated-visual-v14.bundle`
- Bundle SHA-256: `c9f4d2aa4692789c9f02ff71f7126089e9ceebb856de7d9cef9a7f6458b72a66`
- Source ref: `codex/cu-isolated-guest-bootstrap-v1`
- Source cutoff: `0137969dcfbb7453dd716d1ed1894e4cfc7334b9`
- Source parent: `420b8bbbc1a7d37cc58e9db60d4bbaeedcdf530c`
- Developer checkout (must remain untouched):
  `6409645cb7d0fe6d75585f0610366340f808b8ec`

The bundle has complete history and passes `git bundle verify`. The handoff
file is documentation added after the bundle was sealed; the bundle is the
only source input for the campaign.

## Copyable external prompt

```text
Run the GrokPtah Stage 9 / #288 isolated visual Computer Use qualification
campaign from this exact immutable bundle. This is a fail-closed qualification
procedure, not an implementation task:

Bundle: /private/tmp/grokptah-cu-isolated-visual-v14.bundle
Bundle SHA-256: c9f4d2aa4692789c9f02ff71f7126089e9ceebb856de7d9cef9a7f6458b72a66
Source cutoff: 0137969dcfbb7453dd716d1ed1894e4cfc7334b9

Create a disposable checkout, verify bundle SHA/complete history/exact HEAD,
and keep the developer checkout, existing sessions, Git branches, GitHub, and
the running b250b70 campaign untouched. Do not infer VM capability from source
tests, Linux image reproducibility, package signing, or launchAttempted=false.

For every Rust command set exactly:
RUSTC_WRAPPER=/opt/homebrew/bin/sccache
SCCACHE_DIR=/Users/chriscase/Library/Caches/grokptah/sccache
CARGO_TARGET_DIR=/Users/chriscase/Library/Caches/grokptah/targets/rust-1.92.0-stage5-memory-default
Reuse that target serially. Before building report OS/build,
Virtualization.framework availability, non-ad-hoc signing identity fingerprint
(never private material), free disk, active cargo/rustc owners, target path and
owner. Afterward report target size, lsof/open handles, and cleanup/retention.
Abort if any required host capability or exact disposable-output precondition
is absent.

Follow docs/COMPUTER_USE_ISOLATED_QUALIFICATION_RUNBOOK.md exactly:
1. Run the repository-owned guest/helper source verifiers.
2. Build the pinned guest image twice on the pinned Linux runner and compare
   image and manifest bytes.
3. On a credentialed macOS host, package the reviewed image using the
   repository-owned signed-app script. Verify helper-only App Sandbox +
   Virtualization entitlements, no network/share/clipboard/credential/
   host-input/USB/camera/microphone devices, matching identities, exact
   manifest digests, and strict deep signing. Failed packaging leaves no
   promoted output.
4. Run the packaged supervisor from the signed GrokPtah app bundle. Retain
   secret-free evidence for Prepared -> Running -> Bound -> Stopping ->
   CleanupPending -> Terminated; frame sequence/dimensions/digest/freshness;
   pointer move/click/scroll/drag, keyboard navigation and Unicode text;
   stale/replay/wrong-challenge/malformed-input/duplicate-held-state rejection;
   helper kill, guest crash, VM-start failure, timeout, Stop, Take over,
   restart, and exact PID/descriptor/overlay/frame-cache cleanup.
5. Prove the host foreground app, active window, physical pointer, clipboard
   digest, and unrelated windows are unchanged before/after every interaction.
   Uncertain input poisons the surface; never retry or resume automatically.
6. Obtain independent security, accessibility/hardware, and expert UI review
   on this exact packaged identity.

If any required item is unavailable, return NOT QUALIFIED and stop. Do not
enable HostNative or VisualFallbackAct, close #288, or infer a hardware claim.
Return one dated, secret-free report with exact source/package/guest/
configuration/manifest digests, every stage result, negative cases, cleanup
ownership, reviewer roles, and an explicit QUALIFIED or NOT QUALIFIED decision.
Attach it only to source cutoff 0137969d… .
```

## Interpretation

Only a complete report covering signed packaging, real VM boot, rendered
frames, isolated host input, takeover, restart, cleanup, and independent review
may advance Stage 9/#288. A source pass, package-signature pass, or
`launchAttempted: false` remains **NOT QUALIFIED**.
