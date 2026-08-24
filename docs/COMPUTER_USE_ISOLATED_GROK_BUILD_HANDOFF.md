# Isolated visual Computer Use — external Grok Build handoff

Status: **qualification procedure only; no VM capability is claimed by this document.**

This is the copyable handoff for the credentialed, long-running Stage 9 / #288 campaign. It
qualifies one immutable source bundle and one packaged macOS identity. A source verifier, Linux
image comparison, or package-signature check must never be reported as a VM boot/render/input/
cleanup result.

## Exact candidate

- Source bundle: `/private/tmp/grokptah-cu-isolated-visual-v8.bundle`
- Bundle SHA-256: `d356555b219696040322d4b5a147c3c25a2a2df4d1003405a4544dfb393ac049`
- Source cutoff: `2b781421a2f8bdddda918b6b3f94c8651aca5b97`
- Branch: `codex/cu-isolated-guest-bootstrap-v1`
- Docs-only handoff checkpoint (contains the evidence verifier): `2b781421a2f8bdddda918b6b3f94c8651aca5b97`
- Main checkout must remain untouched at `6409645cb7d0fe6d75585f0610366340f808b8ec`.

## Paste this to the external build owner

```text
Run the isolated visual Computer Use qualification procedure for GrokPtah Stage 9 / #288.
This is a fail-closed certification campaign, not an implementation task.

Use only the exact source bundle and SHA below. Create a disposable checkout; do not modify the
developer checkout, existing app sessions, Git branches, or GitHub. Do not infer a VM capability
from source tests, a Linux image comparison, package signing, or launchAttempted=false.

Bundle: /private/tmp/grokptah-cu-isolated-visual-v8.bundle
Bundle SHA-256: d356555b219696040322d4b5a147c3c25a2a2df4d1003405a4544dfb393ac049
Source cutoff: 2b781421a2f8bdddda918b6b3f94c8651aca5b97

Before any build, report macOS version/build, Virtualization.framework availability, signing
identity fingerprint (never private material), free disk, active cargo/rustc processes, intended
target path, and exact owner. Abort if the identity is absent, the output path exists, another
process owns the target, or disk headroom is insufficient.

For every Rust command, set these exact variables and use the family target serially:
RUSTC_WRAPPER=/opt/homebrew/bin/sccache
SCCACHE_DIR=/Users/chriscase/Library/Caches/grokptah/sccache
CARGO_TARGET_DIR=/Users/chriscase/Library/Caches/grokptah/targets/rust-1.92.0-stage5-memory-default
Report target size and cleanup/no-cleanup status afterward. Do not create an in-checkout target.

Run the repository-owned guest/helper verifiers, the read-only evidence verifier, and the pinned
Linux guest reproducibility workflow exactly as documented in
docs/COMPUTER_USE_ISOLATED_QUALIFICATION_RUNBOOK.md. Build the guest image twice and compare image
and manifest bytes. If any check fails, retain a secret-free failure record and do not package.

On the credentialed macOS host, package the exact reviewed image with the repository-owned
package-signed-app.sh. Verify helper-only App Sandbox + Virtualization entitlement, no network,
shared directory, clipboard, credential, host-input, USB, camera, or microphone surface, matching
bundle/team identity, deep strict signature, and exact manifest digests. A failed package attempt
must leave the requested output path absent.

Then run the packaged IsolatedVisualPackagedRuntime campaign from the signed GrokPtah app. Record
secret-free evidence for Prepared -> Running -> Bound -> Stopping -> CleanupPending -> Terminated,
frame sequence/digest/freshness, pointer move, click, scroll, drag, keyboard navigation, Unicode
text, visual postconditions, stale/replay/wrong-secret/malformed-input rejection, guest crash,
helper kill, VM start failure, timeout, Stop, Take over, restart, and exact process/open-handle/
overlay/frame-cache cleanup. Before/after host foreground app, active window, physical pointer,
clipboard digest, and unrelated-window visibility must be unchanged.

Run the independent security, accessibility/hardware, and expert UI review. Keep host paths,
credentials, channel secrets, raw screenshots/logs, and private signing material out of retained
evidence. If any required item is missing, report NOT QUALIFIED and do not close #288 or enable
HostNative/VisualFallbackAct. Never retry uncertain guest input or resume after restart.

Return one dated, secret-free report containing the exact candidate/package digests, every required
stage result, negative-case results, cleanup evidence, reviewer identities/roles, and an explicit
QUALIFIED or NOT QUALIFIED decision. Attach it only to the exact candidate SHA.
```

## Required handoff artifacts

The report must include the exact bundle and package digests, signed identity/team fingerprints,
guest image and manifest comparison, lifecycle events, frame/input acknowledgements, host-integrity
before/after records, negative cases, cleanup ownership checks, and independent security/UI review.
It must explicitly state whether `launchAttempted` occurred. A report that stops before a signed VM
launch remains **NOT QUALIFIED**.

After the campaign, run `docs/verify-isolated-runtime-evidence.sh` from the docs-only handoff
checkpoint (`2b781421…`) or another checkout that contains that exact script. The immutable source
bundle is the current candidate at `2b781421…`; the verifier is a source-level evidence aid and
must not be treated as a VM qualification result. Then update `ROADMAP_TO_100.md`, `CAPABILITY_MATRIX.md`, and
`COMPUTER_USE_ISOLATED_RUNTIME_EVIDENCE.md` together. Do not change an unsupported or planned
status based on this handoff alone.
