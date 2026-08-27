# Computer Use packaged macOS authority (#444)

This document is the topology and eligibility index for packaged Computer Use
identity. It does not close #444. Simulator, cargo-run, and ad-hoc signing are
not packaged qualification.

## Exact source

- Required `origin/main` gate: `67e29bd34dc64049432c715c93c2cef2185c63ea`
- Historical donor heads #289 `d09cffce7a57305352f1d0659490a54752a17970` and
  #290 `71186f1ad0b9f107f804ec6feba194c2a4fe182f` are ancestors of that SHA and
  were audited, not merged wholesale.

## Declared identities

| Role | Bundle ID | Version | Minimum OS |
|---|---|---|---|
| Packaged app | `com.chriscase.grokptah` | `0.1.0` | 11.0 |
| Computer Use helper | `com.chriscase.grokptah.computer-use-helper` | `0.1.0` | 14.0 |
| Disposable demo target | `com.chriscase.grokptah.computer-use-demo` | fixture | 14.0 |

Canonical machine-readable copy:
`docs/schemas/grokptah-computer-use-package-identity.v1.json`.

Declared helper nest path:
`GrokPtah.app/Contents/Helpers/GrokPtah Computer Use Helper.app`.

App/helper major.minor must match. A newer helper major or a skewed minor is
rejected.

## Current runtime vs declared packaged topology

On this branch the native adapter is still an **in-process host**:

- Tauri executable `grokptah-desktop`
- Objective-C shim statically linked into `grokptah-agent-bridge`
- TCC principal of a live observe/act is the **current process**, not a helper
- `ComputerPlatformStatus.executor.kind` reports `in_process_host`

The helper bundle ID, Info.plist, and empty entitlements are **declared
identities**. They are not an assembled, signed helper binary. Packaged
qualification therefore remains ineligible until a notarized helper is the TCC
principal and the hardware fixtures run against that identity.

## Entitlements

`desktop/src-tauri/macos/GrokPtah.entitlements` and
`ComputerUseHelper.entitlements` are empty by design:

- no App Sandbox
- no Apple Events
- no Keychain access groups
- no private entitlements

Screen Recording and Accessibility are TCC, not entitlements. This slice never
requests credentials or Keychain passwords as a workaround.

## Signing classes that cannot count as packaged

`uninspected`, `unsigned`, `ad_hoc`, `apple_development`, and unsigned
`developer_id` without notarization **cannot** satisfy #444. Only
`notarized_developer_id` plus a assembled helper, helper TCC grants, and the
real semantic hardware scenario may count.

## Synthetic acceptance oracle

`computer_use::qualification::run_synthetic_oracle` covers:

permission missing / granted / revoked; valid semantic observe→approve→act→reobserve;
stale target; secure field; takeover race; helper crash before injection; helper
crash after injection before receipt; duplicate dispatch ID; two recovery
restarts; exact cleanup.

Those fixtures prove the helper **contract**. They do not prove a packaged app.

## Qualification command

Source/identity inspection (no TCC, no signing, no process kill):

```sh
cd desktop
npm run qualify:computer-use-package
```

Optional inspect of an already-built app, still not packaged proof:

```sh
GROKPTAH_PACKAGE_APP=/path/to/GrokPtah.app npm run qualify:computer-use-package
```

Focused library contract tests:

```sh
cargo test --locked --manifest-path crates/codegen/grokptah-agent-bridge/Cargo.toml \
  --lib computer_use::package_identity computer_use::helper_authority computer_use::qualification \
  -- --test-threads=1
```

## Continuation when capacity and credentials exist

This host was below the 20 GiB free disk gate, so no unsigned package assembly
and no real TCC/hardware action ran. When **all** of the following are true:

1. at least 20 GiB free,
2. no protected shared Computer Use target is occupied,
3. a Developer ID signing identity is available **without** unexplained Keychain
   prompts (operator-controlled),
4. Screen Recording and Accessibility are already granted to the **packaged
   helper** identity,

continue with:

```sh
cd desktop && npm run tauri:build
GROKPTAH_PACKAGE_APP=src-tauri/target/release/bundle/macos/GrokPtah.app \
  npm run qualify:computer-use-package
# only then, with already-granted helper TCC, run the disposable demo fixture
# through the packaged app identity. Never treat terminal grants as packaged.
```

Do not merge, notarize, or close #444 from this document.
