# Isolated visual runtime-session source evidence

Status: source-level progress only; this record does not qualify a packaged
Computer Use backend or satisfy the #288 release gate.

## Candidate identity

- Branch: `codex/cu-isolated-guest-bootstrap-v1`
- Head: `d488aa4c31e8992f1537a4956e43e7bfcef2acdc`
- Bundle: `/private/tmp/grokptah-cu-stage10-runtime-session-v1.bundle`
- Bundle SHA-256: `bdd76c7ceaa910e261d4805bd52bc4172c054f5e7ebc060ac2688839d43ca5b0`
- Base checkout: main remains clean at `6409645cb7d0fe6d75585f0610366340f808b8ec`

Current sealed branch head: `d34f7654fceb29e5164468108e74b6a414a51aa4`.

The later guest-input validation extension is sealed at:

- Commit: `5c5f21457d200b45dcad9ddd9d5bede0500344fd`
- Bundle: `/private/tmp/grokptah-cu-stage13-guest-input-validation-v1.bundle`
- Bundle SHA-256: `5c9e8c98e9e23d93cf926b32c456eb08339db9f527c5614534622e41bec8c67f`

The guest held-input enforcement extension is sealed at:

- Commit: `d34f7654fceb29e5164468108e74b6a414a51aa4`
- Bundle: `/private/tmp/grokptah-cu-stage14-guest-input-state-v1.bundle`
- Bundle SHA-256: `16abf1113ae91dbcc436e0999a2412313d1d2191bb6e96638ef3f19c09e9a123`

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
- The freestanding guest source validates the authenticated input packet,
  sequence fence, identity-bound HMAC, coordinate/key/text bounds, and closed
  message kind set after binding. It intentionally has no valid frame source
  yet, so input remains fail-closed until guest capture is implemented.
- The guest independently tracks held mouse-button and key state: duplicate
  downs, mismatched releases, invalid key transitions, and shutdown with any
  held input fail closed. Binding resets the held-state fence, and the
  protocol self-test covers both button and key transitions.
- Source tests exercise the bound transition, frame-carrier round trip,
  frame-fenced input packet, stop transition, and pre-binding rejection paths.

## Safe validation performed

- `cargo fmt --manifest-path crates/codegen/grokptah-agent-bridge/Cargo.toml --all -- --check`
- `cargo metadata --locked --offline --no-deps --format-version=1`
- `desktop/src-tauri/macos/isolated-visual-guest/verify-guest-source.sh`
- `desktop/src-tauri/macos/isolated-visual-helper/verify-helper-source.sh`
- `git diff --check`

The guest verifier reported its protocol self-test and closed-source checks
passing. The helper verifier linked an arm64 Mach-O against
`Virtualization.framework` and passed its entitlement/configuration/source
checks.

## Explicit nonclaims

No local cargo test/check/clippy campaign was run under the controlled build
policy. No packaged helper, signed app, reviewed guest image, host process
supervisor, VM launch, socket frame renderer, input dispatch, hardware matrix,
soak campaign, or `HostNative` capability claim exists yet. The bundle is a
reviewable source candidate, not release evidence.

## Next required gates

1. Run the controlled Rust qualification campaign and independent review.
2. Produce and review the deterministic guest image, signed helper, and exact
   packaged app on a credentialed macOS host.
3. Wire the packaged supervisor to this coordinator and run lifecycle,
   restart, resource, security, hardware, and accessibility campaigns.
4. Connect the guest capture/render loop and one-action-approved input path;
   only then consider the #288 acceptance criteria and native dispatch.
