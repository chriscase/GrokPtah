# Isolated visual Computer Use qualification runbook

Status: **external qualification procedure; not a passed campaign**.

This runbook is the operational handoff for Stage 9 / [#288](https://github.com/chriscase/GrokPtah/issues/288).
It deliberately separates source checks, guest-image reproducibility, package identity, and actual
VM behavior. Passing an earlier section never authorizes the next section or creates a capability
claim.

## Candidate and evidence rules

- Qualify one immutable candidate SHA and record it before any build.
- Keep the development checkout and all existing app sessions untouched.
- Use a disposable qualification checkout and exact output paths.
- Never include signing identities, channel challenges, host paths, credentials, or raw guest logs
  in retained evidence.
- A source verifier, Linux image comparison, or package signature check is **not** a VM boot proof.
- Do not enable `HostNative` or `ComputerUseTier::VisualFallbackAct` from this runbook; dispatch
  remains disabled until independent security review and the complete campaign pass.

## 1. Host preflight

Record the candidate SHA, OS/build versions, disk headroom, and active process ownership. Abort if
another build owns the intended target, if the signing identity is absent, or if the exact output
directory already exists.

For Rust commands, use the repository-family cache policy explicitly and serially:

```sh
export RUSTC_WRAPPER=/opt/homebrew/bin/sccache
export SCCACHE_DIR=/Users/chriscase/Library/Caches/grokptah/sccache
export CARGO_TARGET_DIR=/Users/chriscase/Library/Caches/grokptah/targets/rust-1.92.0-stage5-memory-default
```

The campaign must report `df`/process ownership before building and target size/cleanup status
afterward. Do not create an in-checkout or per-agent Cargo target.

## 2. Source and guest reproducibility gate

Run the source checks in the disposable checkout:

```sh
desktop/src-tauri/macos/isolated-visual-guest/verify-guest-source.sh
desktop/src-tauri/macos/isolated-visual-helper/verify-helper-source.sh
```

On the pinned Linux qualification runner, fetch the exact lock-file source and build it twice using
the checked-in workflow. Retain only the image/manifest SHA-256 record and the workflow run identity:

```sh
desktop/src-tauri/macos/isolated-visual-guest/fetch-kernel-source.sh \
  /absolute/runner-temp/linux.tar.xz
desktop/src-tauri/macos/isolated-visual-guest/build-guest-image.sh \
  /absolute/runner-temp/linux.tar.xz \
  /absolute/runner-temp/Image \
  /absolute/runner-temp/manifest.json
```

Do not replace either script with an ad-hoc `curl`, `tar`, kernel build, or
artifact copy. The fetch script enforces the locked HTTPS source, redirect,
time, size, and digest gates; the builder stages image/manifest outputs and
publishes them only after the complete manifest is written. If either command
fails, verify that its requested final output paths are absent before retrying.

Build two isolated outputs and compare both image and manifest bytes. If either differs, stop and
retain a failed reproducibility record; do not package either output.

## 3. Signed package gate

On a credentialed macOS host, use a non-ad-hoc signing identity and a fresh output path:

```sh
desktop/src-tauri/macos/isolated-visual-helper/package-signed-app.sh \
  /absolute/input/GrokPtah.app \
  /absolute/output/GrokPtah-isolated-candidate.app \
  /absolute/reviewed/Image \
  <reviewed-guest-image-sha256> \
  <non-ad-hoc-signing-identity>
```

Retain the script's helper/app/guest/configuration digests, designated-requirement digest, signing
identity fingerprint (not private material), and `codesign --verify --deep --strict` result. Verify
that:

- the outer app has no virtualization entitlement;
- only the helper has the exact App Sandbox + Virtualization entitlements;
- no network, shared-directory, clipboard, credential, host-input, USB, camera, or microphone
  device is configured;
- the embedded guest/configuration bytes match the reviewed manifest;
- helper and outer-app code identities/team requirements match the release policy.

The package script assembles under a disposable staging app and publishes the
requested output only after all checks pass. If it fails, the requested output
path must remain absent; do not promote a staging directory manually.

If any check is unavailable, the result is `not qualified`, not “best effort.”

## 4. Packaged runtime campaign

The candidate supervisor must be exercised **from the correctly signed GrokPtah app bundle**, not
from a standalone example or unsigned helper. The harness must retain a secret-free record for each
run containing:

1. exact app/helper/guest/configuration/manifest digests;
2. `Prepared → Running → Bound → Stopping → CleanupPending → Terminated` events;
3. per-frame sequence, dimensions, digest, and freshness result;
4. guest cursor/input acknowledgement and postcondition for pointer move, click, scroll, drag,
   keyboard navigation, and Unicode text;
5. host foreground app, active window, physical pointer, and clipboard digest before/after;
6. negative cases: stale frame, replay, wrong challenge, malformed input, duplicate held state,
   helper kill, guest crash, VM start failure, timeout, takeover, and app restart;
7. exact helper PID/open-handle/overlay/frame-cache checks before and after cleanup.

The host pointer, foreground application, active window, clipboard digest, and unrelated visible
windows must remain unchanged. Any uncertain input poisons that surface; it must never be retried or
resumed automatically.

## 5. Promotion decision

Before attaching a source handoff, run the read-only evidence consistency check:

```sh
docs/verify-isolated-runtime-evidence.sh
```

It validates the documented source cutoff, bundle digest and complete history,
and the explicit nonclaims. A failed evidence check is a handoff failure, not
permission to infer a qualification result.

Only an independently reviewed report containing every item above may recommend closing #288 or
granting isolated visual dispatch. A report that ends at source validation, package signing, or
`launchAttempted: false` remains an implementation candidate. Attach the report to the exact
candidate SHA and update [`ROADMAP_TO_100.md`](ROADMAP_TO_100.md),
[`CAPABILITY_MATRIX.md`](CAPABILITY_MATRIX.md), and
[`COMPUTER_USE_ISOLATED_RUNTIME_EVIDENCE.md`](COMPUTER_USE_ISOLATED_RUNTIME_EVIDENCE.md) together.
