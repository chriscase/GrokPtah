# Computer Use on macOS

The first native Computer Use adapter is read-only and requires macOS 14 or later. GrokPtah
itself keeps its macOS 11 minimum: ScreenCaptureKit is loaded from its fixed system-framework path
only on a supported OS, and every native entry point checks runtime availability before use.

## Consent and selection

Ordinary startup calls only the non-prompting Screen Recording and Accessibility preflight APIs.
The desktop exposes separate **Request** buttons under Settings > Computer Use. Window discovery
and capture are separate, explicit local actions. No target is selected from a model prompt, MCP
request, remembered native window ID, or application title.

The picker returns one-use, two-minute selection tokens for at most 128 windows. Binding consumes
the token and revalidates the exact ScreenCaptureKit window ID, process ID, and bundle ID. The
adapter excludes GrokPtah, login/lock UI, SecurityAgent, authorization hosts, and System Settings.
It never exports window titles. Starting any new discovery attempt invalidates the prior picker
snapshot, including when the new discovery fails.

Permission states remain distinct: missing, prompt pending, denied, granted, revoked, restricted,
and unsupported. A first macOS grant may require restarting the app before the preflight API sees
the new state. Revocation fails subsequent discovery or observation closed.

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
  per side and 64 MiB raw; the desktop one-shot flow permits at most 4 MiB encoded evidence.
- Evidence stays in process memory. The desktop reads it through the current run and exact asset
  hash, returns one preview, then cancels the run and destroys the backend copy.
- Leaving the Computer Use section clears target and preview state. A capture that finishes after
  the section closes is discarded rather than repopulating the next view.

Custom-drawn password controls that do not expose a secure Accessibility role cannot be reliably
identified by any AX-based redactor. Such applications should not be selected until an adapter or
application-specific policy can attest their sensitive regions.

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

It creates a temporary signed-by-the-host `.app` with normal text, a secure text field, a button,
and a scroll area. In GrokPtah, open a session, then use Settings > Computer Use:

1. Check that opening the section does not prompt.
2. Use each Request button and complete the macOS prompt locally.
3. Refresh status, find windows, and choose **GrokPtah Computer Use Demo**.
4. Observe once. Verify the secure field value is absent and its pixels are blacked out.
5. Move and resize the fixture, observe again, then minimize and quit it to verify the distinct
   geometry, target-closed, and hidden-window paths.

The adapter has no input-event function. The smoke must not type, click, scroll, or activate the
fixture through GrokPtah.
