# Isolated visual helper source contract

This directory is an unshipped Stage 9 candidate. It defines the smallest macOS helper bootstrap
that can own Virtualization authority without giving that entitlement to the GrokPtah desktop
process. It is not a guest, frame carrier, input backend, signed package, or #288 proof.

## Inherited descriptors

The helper accepts no arguments and clears its inherited environment before initializing the VM.
The future host supervisor must supply exactly these already-verified descriptors:

| FD | Access | Meaning |
|---|---|---|
| 3 | read only | immutable embedded-initramfs Linux kernel image |
| 4 | read only | exact `grokptah-isolated-config-v1.json` bytes |
| 5 | read only pipe/socket | private host control channel |
| 6 | write only pipe/socket | private helper event channel |

FDs 3 and 4 must be nonempty regular files, non-executable, not group/world writable, and within the
32 GiB / 1 MiB ceilings. The helper rewinds them before use. FDs 5 and 6 must be private pipes or
sockets with the exact direction shown. Paths, secrets, environment values, arbitrary log text, and
model/provider traffic never cross this bootstrap ABI.

## Closed bootstrap protocol

Control is one byte:

- `0x01`: start once, accepted only after the helper emits `prepared`;
- `0x02`: stop, accepted only after `running`.

Every event is exactly 16 bytes in network byte order:

| Bytes | Field |
|---|---|
| 0–3 | magic `0x47505449` (`GPTI`) |
| 4–5 | version `1` |
| 6–7 | event: prepared `1`, running `2`, stopped `3`, failure `4` |
| 8–11 | closed numeric failure detail, or zero |
| 12–15 | reserved zero |

Unknown/early control, EOF, invalid descriptors/configuration, unavailable Virtualization,
unexpected guest stop, and bounded shutdown failure fail closed. SIGINT/SIGTERM take the same stop
path. Graceful shutdown gets two seconds before a destructive stop; destructive stop gets ten
seconds. The parent must still prove exact process/handle cleanup before deleting any per-Run files.

The fixed event bytes and control values are shared with the freestanding guest protocol header.
The bridge contains a host-supervisor codec/state machine that accepts only the prepared → start →
running → stop → stopped sequence (or one terminal failure). It does not spawn a helper, hold a
descriptor, or mint an isolated capability; it is a pre-runtime ABI seam so a future supervisor
cannot silently accept reordered, unknown, or post-terminal events.

The start path configures one bounded graphics scanout, entropy, and virtio socket. Network, shared
directories, audio, storage, keyboards, pointing devices, and serial devices are explicitly empty.
The helper performs a bounded challenge/response with the guest bootstrap agent before emitting
`running`, and requires an authenticated shutdown acknowledgement before terminal success. The
guest agent—not the host pointer or clipboard—must eventually own framebuffer capture and guest-local
input through the authenticated protocol documented in `docs/COMPUTER_USE_ISOLATED_VISUAL.md`;
those carrier and input paths are not implemented here.

## Build and package boundaries

- `build-helper.sh` links an **unsigned** SDK/source-check binary. CI runs its invalid-start path; CI
  does not embed it in GrokPtah.
- `package-signed-app.sh` requires a reviewed guest digest and non-ad-hoc identity, signs the helper
  with `isolated-visual-helper.entitlements.plist`, writes the measured manifest, then signs the outer
  app with `grokptah-main.entitlements.plist` (empty).
- The runtime verifier requires strict nested signing, exact helper/app identity, matching team,
  hardened runtime/library validation, the helper's designated-requirement digest, and the exact
  entitlement/content boundary.

The repository now carries a pinned Linux arm64 guest-source lock, a closed kernel fragment, a
freestanding guest PID 1, and a Linux-only deterministic image-builder candidate under
`../isolated-visual-guest/`. The builder is exercised only by the dedicated Linux CI workflow; it
is intentionally not run on this macOS host. There is still no reviewed guest artifact or valid
signing identity on the development host, so the credentialed assembler has not been run and no
package/runtime claim exists.
