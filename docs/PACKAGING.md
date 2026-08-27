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
`bundle.macOS.entitlements` points at the empty
`desktop/src-tauri/macos/GrokPtah.entitlements` file so the packaged identity is
inspectable. That file is not App Sandbox, Apple Events, or a Computer Use helper
binary. Ad-hoc or unsigned GrokPtah.app output cannot satisfy #444.

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
