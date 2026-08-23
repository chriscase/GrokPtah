# Computer Use on macOS

The first native Computer Use adapter observes one exact window and performs a deliberately small
set of semantic Accessibility actions on macOS 14 or later. Its established path is
**foreground-semantic**: it operates the real foreground OS application and may activate it. A
separate Stage 8 candidate can bind one exact, locally measured background text field after a
reversible disposable-target calibration. That narrow path does not make the foreground adapter
or arbitrary Accessibility actions background-safe. Neither path is an isolated visual input
domain, and neither may be advertised as isolated from pointer, key, clipboard, or focus effects.
Distinct window IDs are selection/target identity. Native macOS is one
host-global-foreground conflict domain (capacity 1); two windows share one physical input domain
and must not be treated as isolated surfaces. Packaged hardware focus, TCC, and takeover evidence remain explicitly
unverified. Takeover is durable bookkeeping-safe; it is not physically preemptive once an action is
already inside the native action gate. GrokPtah itself keeps its macOS 11 minimum: ScreenCaptureKit is loaded
from its fixed system-framework path only on a supported OS, and every native entry point checks
runtime availability before use.

## Consent and selection

Ordinary startup calls only the non-prompting Screen Recording and Accessibility preflight APIs.
The Computer Run cockpit exposes separate **Request** buttons before native window discovery.
Discovery, exact-window selection, scope review, and run start are separate, explicit local
actions. No target is selected from a model prompt, MCP request, remembered native window ID, or
application title.

The picker returns one-use, two-minute selection tokens for at most 128 windows. Binding consumes
the token and revalidates the exact ScreenCaptureKit window ID, process ID, and bundle ID. The
adapter excludes GrokPtah, login/lock UI, SecurityAgent, authorization hosts, and System Settings.
It never exports window titles. Starting any new discovery attempt invalidates the prior picker
snapshot, including when the new discovery fails.

Permission states remain distinct: missing, prompt pending, denied, granted, revoked, restricted,
and unsupported. A first macOS grant may require restarting the app before the preflight API sees
the new state. Revocation fails subsequent discovery, observation, or action closed. Native
discovery remains disabled until both required permissions report `granted`.

## Observation boundary

- Capture is limited to one selected `SCWindow`; desktop, audio, microphone, and cursor capture are
  disabled.
- Accessibility data is matched to exactly one same-process AX window. Ambiguous or changed
  matches fail closed.
- Secure Accessibility values are omitted. Their target-relative frames are blacked out in the
  in-memory bitmap before PNG encoding.
- If the bounded Accessibility walk truncates, exceeds its visited-node bound, or encounters an
  unexpected partial-tree error, the semantic snapshot may still be returned but the screenshot
  is withheld because complete redaction cannot be proven.
- Element IDs are fresh per observation. Old screenshot asset IDs are removed when a new
  observation starts.
- Captures are serialized and limited to two per second. The native image is capped at 4096 pixels
  per side and 64 MiB raw; the desktop one-shot preview permits at most 4 MiB encoded evidence and
  a bounded native Computer Run permits at most 16 MiB cumulatively.
- Evidence stays in process memory and is addressed through the current run and exact asset hash.
  Reobservation rotates the prior asset, and cancel/stop destroys the backend copy. Durable run
  records contain only bounded metadata, hashes, and opaque asset IDs.
- Leaving the Computer Use section clears target and preview state. A capture that finishes after
  the section closes is discarded rather than repopulating the next view.

Custom-drawn password controls that do not expose a secure Accessibility role cannot be reliably
identified by any AX-based redactor. Such applications should not be selected until an adapter or
application-specific policy can attest their sensitive regions.

## Semantic action boundary

The cockpit can stage `activate target`, Accessibility `invoke`, visible `set value`, `select`, and
semantic `scroll to visible`. Activate target is valid only for this foreground-semantic backend
and is never treated as non-disruptive: `GPTActImpl` still activates with
`NSApplicationActivateIgnoringOtherApps` when that action is explicitly authorized. Background-safe
semantic actions cannot silently activate. The user sees the exact target, action summary, risk, and
visible text payload before granting one use. Each successful or uncertain mutation consumes its
observation; continuing requires a new local authorization and observation.

Immediately before native dispatch, the shim revalidates the exact ScreenCaptureKit window ID,
process ID, bundle ID, frame, frontmost application, focused AX window, semantic traversal index,
role, subrole, label, value, enabled state, and supported action. It rechecks target focus and frame
after dispatch and verifies values where Accessibility exposes a deterministic postcondition.
Permission revocation, app restart, focus theft, window movement, stale element identity, tree
truncation, secure controls, or failed postconditions fail closed and consume the frame.

### Measured background text-entry boundary

The candidate exposes **Measured background text** as a different execution mode, proof, backend
ID, and start command. It supports only `set value` on the exact element calibrated moments before;
foreground activation, invoke, select, scroll, key, pointer, clipboard, and fallback are denied.

The local calibration contract is intentionally visible and destructive only to a disposable
fixture: choose a visible, non-minimized window that is not active; provide the exact label of one
enabled, non-secure, non-sensitive text field with a readable value; acknowledge disposability;
and let GrokPtah write a distinct probe value and restore the original. Native code measures all
of the following around each dispatch:

- frontmost process identity;
- active layer-zero window identity for that process;
- physical pointer coordinates;
- target remains background, visible, non-minimized, and frame-stable;
- exact AX element attestation and value postcondition.

If measurement is unavailable or changes, the result is uncertain and no authority is minted.
Restoration is attempted even when the first mutation's result is ambiguous, but a receipt is
issued only after exact re-observation proves the original value is back. The receipt expires in
two minutes, is consumed once, and binds the picker selection, target generation, native
process/window, and element digest. Runtime actions repeat the same boundary checks and require a
fresh observation plus a one-use local `text_entry` grant.

This candidate does not certify every application or every AX implementation. Custom controls,
write-only values, ambiguous labels, secure fields, sensitive/truncated surfaces, hidden or
minimized windows, and any target that cannot preserve the measured external state are denied.
Packaged native disposable-fixture evidence and strict external qualification remain open, so
[#287](https://github.com/chriscase/GrokPtah/issues/287) must remain open.

This slice has no `CGEventPost` keyboard/pointer injection, coordinate fallback, cursor movement,
clipboard access, AppleScript, secret substitution, automatic approval, unattended mode, model
Computer tool, or Computer mutation over MCP. Capability booleans stay `pointer_fallback=false`
and `key_chords=false`; they are a public projection of a foreground-semantic typed proof and
cannot authorize pointer or key dispatch. Pause, Stop, and Take over revoke authority without
depending on the model or network; an action that loses the durable completion race cannot commit
as successful. The candidate's exact atomic cancellation signal preempts native preflight and
activation waits, but an Accessibility API call already entered is not physically reversible and
remains uncertain and non-replayable.

## Packaging and signing

The Objective-C shim is built only for macOS and links AppKit, ApplicationServices, CoreGraphics,
and Foundation. ScreenCaptureKit is runtime-loaded from `/System/Library/Frameworks` so systems
below macOS 14 can still launch GrokPtah with Computer Use reported as unavailable. The adapter
does not search user-writable framework locations.

Apple's Screen Recording and Accessibility consent APIs used here do not define camera-style
`Info.plist` usage-description keys, so GrokPtah does not invent unsupported keys. The app is not
sandboxed and this slice adds no private entitlement. If App Sandbox is introduced later, native
computer control must be re-reviewed before release.

TCC grants are associated with the application identity. Development binaries that move or change
ad-hoc signatures may need consent again. Release testing should use the same stable Developer ID
signature and bundle identifier as the shipped app, then verify the notarized artifact rather than
assuming a development grant transfers.

References:

- [Apple ScreenCaptureKit capture sample](https://developer.apple.com/documentation/screencapturekit/capturing-screen-content-in-macos)
- [AXIsProcessTrustedWithOptions](https://developer.apple.com/documentation/applicationservices/1459186-axisprocesstrustedwithoptions)
- [CGPreflightScreenCaptureAccess](https://developer.apple.com/documentation/coregraphics/cgpreflightscreencaptureaccess%28%29)
- [Tauri macOS Info.plist extension](https://v2.tauri.app/distribute/macos-application-bundle/)

## Disposable native smoke

Build and launch the repository-owned fixture:

```sh
./evals/macos-computer-use-demo/build-and-run.sh
```

It creates a temporary signed-by-the-host `.app` with normal text, a secure text field, a priority
selector, an actionable button, a status label, and a scroll area. In GrokPtah, open a disposable
session and its Computer Run cockpit:

1. Check that opening the section does not prompt.
2. Use each Request button and complete the macOS prompt locally.
3. Refresh status, find windows, and choose **GrokPtah Computer Use Demo**. Review the exact bundle
   ID and one-action scope before starting.
4. Verify the secure field is absent from the semantic snapshot. Stage and approve activation.
5. Reauthorize and observe, enter a visible bounded project label, and approve once.
6. Reauthorize and observe, invoke **Submit fixture**, and verify the status contains the label and
   selected priority.
7. Move or resize the fixture between observation and approval to verify the action fails closed.
   Minimize and quit it to verify the distinct hidden-window and target-closed paths.

For an opt-in bridge-level smoke against the same fixture, set `GROKPTAH_LIVE_COMPUTER_USE=1` and
run `cargo run --example macos_computer_use_actions` from the bridge crate. It never prompts for
permissions and exits unless the existing Screen Recording and Accessibility grants are already
visible to that process.

For the measured-background candidate, leave the demo visible but put Terminal or another ordinary
app in front. Then set `GROKPTAH_LIVE_BACKGROUND_COMPUTER_USE=1` and run
`cargo run --example macos_computer_use_background_text` from the bridge crate. The smoke refuses
an active, hidden, or minimized demo; performs and restores the calibration probe; binds the exact
one-use receipt; performs two measured runtime text-entry actions; and finally restores the
original disposable value. Each native dispatch independently verifies that the foreground
process, active foreground window, and physical pointer did not change. This opt-in development
smoke is not packaged-identity release evidence by itself.

### Isolated packaged-app inspection

When inspecting a packaged GrokPtah GUI in a disposable run, set `GROKPTAH_HOME` explicitly before
launching the app. Do not rely on `HOME` alone: macOS home-directory resolution can ignore that
variable, which would let a smoke process read the normal user's session store.

```sh
SMOKE_HOME=$(mktemp -d "${TMPDIR:-/tmp}/grokptah-packaged-smoke.XXXXXX")
mkdir -p "$SMOKE_HOME/.grokptah"
HOME="$SMOKE_HOME" \
GROKPTAH_HOME="$SMOKE_HOME/.grokptah" \
GROKPTAH_AGENT_OFFLINE=1 \
  GrokPtah.app/Contents/MacOS/grokptah-desktop
```

Use a stable, uniquely identified app process for GUI inspection and confirm that no other
GrokPtah process is running first. Keep the temporary home bounded and remove only that exact
directory after the process exits. This boundary isolates session metadata and credentials; it
does not grant either macOS permission or bypass the local Computer Run consent flow.
