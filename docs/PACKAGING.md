# Packaging (macOS)

## Build

```sh
cd desktop
npm ci
npm run tauri:build
```

Outputs, from the exact commit checked out:

- `desktop/src-tauri/target/release/bundle/macos/GrokPtah.app`
- `desktop/src-tauri/target/release/bundle/dmg/GrokPtah_<version>_<arch>.dmg`

Unsigned local builds are expected and fine; signing/notarization is a separate,
credentialed concern and is deliberately not wired into the default build.

## Verifying a produced package

```sh
cd desktop/src-tauri/target/release/bundle
DMG=dmg/GrokPtah_0.1.0_aarch64.dmg  # use the exact file emitted by your build
hdiutil imageinfo "$DMG" | grep -E 'Format|Checksum'
hdiutil verify "$DMG"                            # expect: checksum is VALID
file  macos/GrokPtah.app/Contents/Resources/icon.icns  # expect: Mac OS X icon
lipo -archs macos/GrokPtah.app/Contents/MacOS/grokptah-desktop
```

Confirm provenance by building from a clean worktree with no local edits
(`git rev-parse HEAD` plus an empty `git status --porcelain`). Record that SHA
with the artifact. This establishes source provenance; signing, notarization,
and reproducible-build guarantees are separate concerns.

## Isolated visual helper candidate

The Stage 9 candidate has a credentialed **assembler** and a Linux-only guest-image **source
builder**, not a committed binary or a shipped backend. The default unsigned desktop build
deliberately does not include an isolated helper or guest image. macOS CI syntax-checks and links
the helper source and validates the guest source; a separate pinned Linux workflow builds the
guest image twice and compares the outputs. Those checks establish source/build reproducibility
only; they are not packaged identity, boot, or runtime evidence.

The helper source and its closed configuration live under
`desktop/src-tauri/macos/isolated-visual-helper/`. The executable accepts no arguments, clears its
environment, and accepts only four inherited descriptors: immutable guest image, immutable
configuration, host-only control pipe, and host-only event pipe. Its initial Virtualization
configuration has one bounded display and virtio socket, but no network, shared directory, audio,
storage, keyboard, pointing, or serial device. It waits for an explicit one-byte start authorization
and implements bounded graceful-then-forced shutdown. This is a no-input bootstrap; it does not
implement the guest agent, frame carrier, input, overlay, or host supervisor.

On an authorized release machine with a valid non-ad-hoc signing identity and a separately built,
reviewed guest kernel image with an embedded initramfs, assemble a new output app rather than
mutating the unsigned input app:

```sh
desktop/src-tauri/macos/isolated-visual-helper/package-signed-app.sh \
  /absolute/path/to/unsigned/GrokPtah.app \
  /absolute/canonical/output/GrokPtah.app \
  /absolute/path/to/grokptah-isolated-guest-v1.img \
  REVIEWED_64_CHARACTER_LOWERCASE_GUEST_SHA256 \
  "Developer ID Application: reviewed identity"
```

The script always selects the reviewed configuration beside its own source. It refuses
symlinked/noncanonical inputs, guest-digest mismatch, configuration drift,
pre-existing output/artifact targets, ad-hoc signing, and unexpected package locations. It builds
and signs the helper first with only App Sandbox and Virtualization entitlements, copies the
immutable guest/configuration, derives the content and canonical designated-requirement digests,
writes the signed-bundle manifest, signs the outer app with an empty main-process entitlement set,
and runs strict all-architecture nested verification. Timestamping requires the release machine's
normal Apple signing access.
If any step fails, treat the new output path as incomplete and discard that exact output before a
retry; the unsigned input app and source artifacts are never modified.

No valid signing identity is present on the current development host, and the Linux builder has
not produced a reviewed artifact in a release package. The assembler has not produced a claimable
package. Notarization and runtime/destructive lifecycle certification remain separate required
gates.

## Troubleshooting: DMG bundling fails after the `.app` succeeds

Symptom: the release binary and `GrokPtah.app` build, then the `dmg` target
fails. Rule out local packaging state before changing repository configuration.

Tauri writes and runs `bundle_dmg.sh`, which attaches an interstitial disk
image with `hdiutil`. If a previous bundling run was interrupted, that image can
stay mounted, and the next attach fails. The script says so itself:

> The interstitial disk image will likely be mounted and will need to be cleaned
> up manually.

Diagnose and recover:

```sh
ls /Volumes                       # look for a stale dmg.XXXXXX entry
mount | grep '/Volumes/dmg\.'     # confirm it is an attached image
hdiutil detach /Volumes/dmg.XXXXXX   # detach the stale mount, then rebuild
```

Note that other projects on the same machine can leave `dmg.*` mounts behind;
detach only the ones you own.

Other things to check before suspecting the repository:

- `hdiutil` and `osascript` present (`command -v hdiutil osascript`).
- Xcode command line tools selected (`xcode-select -p`).
- Enough free disk space for the compressed image plus the interstitial copy.
- No security tooling blocking `hdiutil attach`.

A DMG failure was tracked as a deferred item on #202. Re-running the exact
command on `d319453` in a clean worktree produced a valid, checksum-verified
7.3 MB DMG (`TAURI_BUILD_EXIT: 0`), and the bundle configuration is unchanged
since that issue's baseline (`323a5be`), so it was recorded as transient rather
than a packaging defect.
