# Isolated visual runtime-session source evidence

Status: source-level progress only; this record does not qualify a packaged
Computer Use backend or satisfy the #288 release gate.

## Candidate identity

- Branch: `codex/cu-isolated-guest-bootstrap-v1`
- Head: `e733c5016f9131f01a8167afcbad5eb37973a06d`
- Bundle: `/private/tmp/grokptah-cu-stage34-measured-descriptor-spawn-v1.bundle`
- Bundle SHA-256: `02c99b875f45839b7fde168e9ced3079ab2670e04a0710bc2df3bbeb85727b9b`
- Base checkout: main remains clean at `6409645cb7d0fe6d75585f0610366340f808b8ec`

Current sealed implementation head: `eb9892e1ba5606c73b66abe60067fea5ae7eafb6`.

The later guest-input validation extension is sealed at:

- Commit: `5c5f21457d200b45dcad9ddd9d5bede0500344fd`
- Bundle: `/private/tmp/grokptah-cu-stage13-guest-input-validation-v1.bundle`
- Bundle SHA-256: `5c9e8c98e9e23d93cf926b32c456eb08339db9f527c5614534622e41bec8c67f`

The guest held-input enforcement extension is sealed at:

- Commit: `d34f7654fceb29e5164468108e74b6a414a51aa4`
- Bundle: `/private/tmp/grokptah-cu-stage14-guest-input-state-v1.bundle`
- Bundle SHA-256: `16abf1113ae91dbcc436e0999a2412313d1d2191bb6e96638ef3f19c09e9a123`

The runtime-driver integration extension is sealed at:

- Commit: `7823283284a484f5586edfc79a9dd2109d2a98b1`
- Bundle: `/private/tmp/grokptah-cu-stage15-runtime-driver-v1.bundle`
- Bundle SHA-256: `5b0f3ca91c9c62b34e500a82a9ad0878541ae2e0714003c8cd6b6786ccdaa8ca`

The helper relay extension is sealed at:

- Commit: `c195ea5b6a5fb4f3f2a12911738baaff4abb6143`
- Bundle: `/private/tmp/grokptah-cu-stage16-helper-relay-v1.bundle`
- Bundle SHA-256: `f06788d22d3c49b4cbf981f20ad050cda00ba21362231541c7b0e3f6b01bc8b3`

The guest framebuffer-capture extension is sealed at:

- Commit: `203de561e7189b8e19f17bc515b094450aa6387b`
- Bundle: `/private/tmp/grokptah-cu-stage17-guest-frame-capture-v1.bundle`
- Bundle SHA-256: `c7c8e1f45b70743e7dfbec19cde73dfec156e851238d29e1df43b5d4b19ac0a8`

The deterministic guest-fixture rendering extension is sealed at:

- Commit: `bf1e1b7f0c27598ce155e2d734a23a6f095010ea`
- Bundle: `/private/tmp/grokptah-cu-stage18-guest-fixture-v1.bundle`
- Bundle SHA-256: `15d033f19c615c53e663ef9257c29e1ee4574ea137decdfe23a6ee80d111c401`

The private guest-challenge channel extension is sealed at:

- Commit: `cadcbff96e663c81117241e8e372c59c2f7da6cd`
- Bundle: `/private/tmp/grokptah-cu-stage19-challenge-channel-v1.bundle`
- Bundle SHA-256: `e0ae9b2a8a0d311e0571c96c33239b47c5ab9cf612ddded1e3f83b461865e870`

The bounded packaged-supervisor source extension is sealed at:

- Commit: `eb9892e1ba5606c73b66abe60067fea5ae7eafb6`
- Bundle: `/private/tmp/grokptah-cu-stage32-packaged-supervisor-v1.bundle`
- Bundle SHA-256: `6c27796170c209c832fe7c6d0dc8f9c1233779ee2e6546e6704f7935a48dadb1`

The helper/guest contract-documentation synchronization is sealed at:

- Commit: `5a51d69a57a66ad72139402fc2a2e3fd9080b9e1`
- Bundle: `/private/tmp/grokptah-cu-stage33-contract-docs-v1.bundle`
- Bundle SHA-256: `3611489a7263f4901e681befed1c49d86fed40a98bd83f30fd1c63b88194f29d`

The measured-descriptor spawn hardening is sealed at:

- Commit: `e733c5016f9131f01a8167afcbad5eb37973a06d`
- Bundle: `/private/tmp/grokptah-cu-stage34-measured-descriptor-spawn-v1.bundle`
- Bundle SHA-256: `02c99b875f45839b7fde168e9ced3079ab2670e04a0710bc2df3bbeb85727b9b`

## What this candidate proves

- The helper control ABI has an explicit `bind` command and authenticated
  `bound` event between guest readiness and terminal stop.
- The freestanding guest accepts the length-bounded binding packet and emits
  an authenticated binding acknowledgement; the helper source validates and
  relays that packet over its private control/VSOCK seam.
- `IsolatedVisualRuntimeSession` couples helper event order to the durable
  lifecycle, challenge-derived frame/input channels, frame freshness, input
  admission, restart poisoning, and cleanup evidence. Debug output redacts the
  challenge and carriers remain outside model-facing projections.
- `IsolatedVisualStream` provides a bounded length-delimited private transport
  seam: it refuses oversized allocations, maps mid-packet EOF to a terminal
  condition, delegates frame authentication/freshness to the runtime session,
  and writes only authenticated input packets.
- `IsolatedVisualHelperControl` binds inherited control/event descriptors to
  the same coordinator, serializing only the start/bind/stop controls and
  accepting only decoded fixed-size helper events.
- `IsolatedVisualRuntimeDriver` joins the helper control adapter and private
  frame/input stream behind one lifecycle-owned API. It prevents a future
  supervisor from advancing those seams through unrelated state machines,
  while still deliberately accepting inherited descriptors rather than
  spawning a process or claiming a packaged VM capability.
- The signed-helper source now validates private FD7/FD8 relay descriptors,
  forwards only bounded host input packets to the guest VSOCK, and forwards
  only bounded guest frame packets to the host. This proves relay plumbing and
  fail-closed bounds, not a rendered frame or a working model-facing Computer
  Use run.
- The freestanding guest now has a bounded `/dev/fb0` capture path: it reads a
  fixed 1280×800×4-byte surface, hashes it, emits authenticated 64 KiB frame
  chunks with fresh UUIDv4 nonces, and advances the frame freshness fence. This
  is source-level capture plumbing only; no reviewed guest image, GUI surface,
  or packaged rendered frame has been qualified.
- The guest fixture renderer now writes a bounded deterministic surface through
  the guest framebuffer before capture. Validated pointer, button, and scroll
  packets update only that fixture's cursor/state; this is a qualification
  surface, not arbitrary guest GUI support or a packaged render proof.
- The helper now writes the generated per-launch challenge to a private FD9
  channel, and the Rust host adapter reads a complete nonzero challenge without
  serializing it. This closes the host-binding input seam while leaving process
  launch and package qualification explicitly unclaimed.
- The native macOS shim now consumes the exact helper, guest-image, and
  configuration descriptors returned by the Rust measurement/receipt verifier,
  rejects descriptors without close-on-exec, rechecks helper identity immediately
  before `posix_spawn`, creates only the five private parent/child channels, and maps the fixed descriptor contract under
  `POSIX_SPAWN_CLOEXEC_DEFAULT`, and routes child stdio to `/dev/null`. The
  macOS Rust supervisor owns the returned PID and descriptors, consumes FD9,
  drives the bounded Prepared → Running → Bound lifecycle, applies bounded
  event waits, force-cleans an unresponsive helper, and creates every runtime
  pipe with close-on-exec set in the parent. This is a packaged-
  supervisor **source candidate**; no signed app has launched it and no VM
  boot/render/input/cleanup result is claimed.
- The packaged supervisor exposes read-only runtime inspection while keeping
  lifecycle-state mutation behind its driver; callers cannot obtain a public
  mutable session reference and bypass the ordered helper protocol.
- The stop boundary now rejects held keyboard/button state, waits for the
  helper to exit, and leaves the lifecycle in `CleanupPending` until explicit
  per-surface process, handle, overlay, and frame-cache evidence completes it.
- Startup protocol, helper-I/O, and guest-event failures now transition the
  session to failure/cleanup-pending and reap or force-stop the child before
  returning the original error; they cannot leave a half-started helper behind.
- Frame-authentication, frame-stream, and input-wire failures take the same
  poison-and-reap path. A user-level held-input stop rejection remains
  retryable because no terminal transition has been committed yet.
- The lifecycle transition from `Stopping` to `CleanupPending` now accepts the
  already-recorded terminal disposition, matching the intended stop/cleanup
  protocol and its deterministic test.
- Before native spawn, the Rust launch seam now opens the exact package through
  the existing read-only measurement/receipt verifier and binds the caller's
  manifest to helper, guest-image, configuration, and designated-requirement
  digests. It passes those still-open measured descriptors to the native shim
  instead of allowing a second path-based artifact open; a structurally valid
  but mismatched manifest therefore cannot reach the child process, and the
  native shim repeats the helper path/signature check immediately before spawn.
- The freestanding guest source validates the authenticated input packet,
  sequence fence, identity-bound HMAC, coordinate/key/text bounds, and closed
  message kind set after binding. Input remains fail-closed until a reviewed
  packaged guest capture establishes the frame-freshness fence.
- The guest independently tracks held mouse-button and key state: duplicate
  downs, mismatched releases, invalid key transitions, and shutdown with any
  held input fail closed. Binding resets the held-state fence, and the
  protocol self-test covers both button and key transitions.
- Source tests exercise the bound transition, frame-carrier round trip,
  frame-fenced input packet, stop transition, and pre-binding rejection paths.

## Safe validation performed

- `rustfmt --edition 2021 --check` on each changed Rust source file
- `cargo fmt --manifest-path crates/codegen/grokptah-agent-bridge/Cargo.toml --all -- --check`
  (reports pre-existing unrelated `xai-grok-pager` whitespace; no such file was
  changed here)
- `cargo metadata --locked --offline --no-deps --format-version=1`
- `desktop/src-tauri/macos/isolated-visual-guest/verify-guest-source.sh`
- `desktop/src-tauri/macos/isolated-visual-helper/verify-helper-source.sh`
- `git diff --check`

The changed Rust files were rustfmt-clean. The guest verifier reported its
protocol self-test and closed-source checks passing. The helper verifier linked
an arm64 Mach-O against `Virtualization.framework` and passed its
entitlement/configuration/source checks, including a native-shim object link and
the exported spawn/free symbols. Its invalid-start smoke now supplies a separate
bounded regular guest descriptor rather than reusing the configuration file.
The repository-wide formatter still reports pre-existing unrelated whitespace
in the xai-grok-pager crate; that code was not modified by this candidate.

## Explicit nonclaims

No local cargo test/check/clippy campaign was run under the controlled build
policy. No packaged helper, signed app, reviewed guest image, VM launch, socket
frame renderer, input dispatch, hardware matrix, soak campaign, or `HostNative`
capability claim exists yet. A host process-supervisor **source candidate** now
exists, but it has not been exercised against a signed package. The bundle is a
reviewable source candidate, not release evidence; the framebuffer extension
proves only bounded source-level capture and frame authentication.

## Next required gates

1. Produce and review the deterministic guest image, signed helper, and exact
   packaged app on a credentialed macOS host.
2. Exercise `IsolatedVisualPackagedRuntime` against that package: launch, boot,
   challenge/bind, render, input, stop, crash, restart, resource, and exact
   overlay/handle cleanup.
3. Run the security, hardware, accessibility, and recurring expert-UI
   campaigns; retain the packaged evidence and independent review.
4. Enable `HostNative` only after the #288 acceptance criteria and the
   controlled Rust qualification campaign pass.
