# Isolated visual guest source candidate

This directory contains the Linux arm64 source candidate for the Stage 9 isolated visual
Computer Use substrate. It is not a guest artifact, signed package, boot proof, frame carrier,
input backend, or `HostNative` dispatch proof.

## Source contract

- `guest-source.lock.json` pins the Linux kernel source URL, version, architecture, and SHA-256.
- `kernel.config.fragment` permits only the bounded arm64/virtio/VSOCK/graphics profile. Network,
  storage, shared filesystems, host input, audio, USB, and credential-bearing kernel surfaces are
  disabled.
- `guest-init.c` is a freestanding Linux arm64 PID 1. It uses raw syscalls only, connects to the
  host over VSOCK port `17001`, authenticates the fixed READY frame with the host challenge, accepts
  zero or more authenticated binding commands, emits a binding acknowledgement for each accepted
  identity, then accepts the fixed STOP byte, emits the authenticated shutdown acknowledgement,
  and powers off. The zero-binding path preserves the current lifecycle smoke contract until a
  host supervisor supplies the per-run packet.
- `protocol.h` is shared by the guest and macOS helper. It contains the fixed bootstrap frame
  format, the freestanding HMAC-SHA-256 implementation, and the fixed session-binding header.
  The binding hashes Run, surface, incarnation, and isolated input-domain identities with
  length-prefixed fields, then derives a challenge-bound channel key and confirmation tag. It does
  not carry paths, model traffic, or reusable secrets.
- `isolated_visual_channel.rs` mirrors that canonical digest/key/confirmation contract for the host
  bridge, encodes the fixed binding header plus its four identity fields, and supplies challenge-
  bound constructors for the Rust frame/input carriers. It is a source contract only: the
  helper/guest socket loop still does not consume it.

## Reproducible Linux workflow

On the pinned Linux CI runner, the workflow performs the following bounded sequence:

```sh
desktop/src-tauri/macos/isolated-visual-guest/verify-guest-source.sh
desktop/src-tauri/macos/isolated-visual-guest/fetch-kernel-source.sh \
  /absolute/runner-temp/linux.tar.xz
desktop/src-tauri/macos/isolated-visual-guest/build-guest-image.sh \
  /absolute/runner-temp/linux.tar.xz \
  /absolute/runner-temp/Image \
  /absolute/runner-temp/manifest.json
```

The checked-in workflow builds twice from the same pinned source and compares both `Image` and
manifest bytes. It uploads only short-lived candidate evidence. The macOS desktop and release
workflows validate the source but do not fetch, embed, sign, or publish a guest image.

## Current proof boundary

Local validation proves shell syntax, lock-file shape, protocol/self-test vectors (including the
cross-language binding digest and challenge-derived confirmation), arm64 guest syntax, the closed
kernel fragment, Objective-C helper syntax/linking, and the helper's invalid-start event path. It
does not prove Linux CI execution, kernel boot, VSOCK operation on a real VM, framebuffer capture,
guest application input, cleanup under crashes, signing, notarization, hardware support, or the
#288 acceptance campaign. A CI image comparison must not be promoted to a reviewed release guest
without those additional gates.
