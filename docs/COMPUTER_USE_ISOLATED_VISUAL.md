# Isolated Visual Computer Use

This document records the Stage 9 substrate decision and proof boundary for
[#288](https://github.com/chriscase/GrokPtah/issues/288). It is an implementation decision, not
evidence that isolated visual Computer Use ships today. The issue remains open until the packaged
backend, destructive cleanup drills, visual fixture campaign, and independent security review all
pass on the exact release candidate.

## Decision

Use a **disposable virtual machine with no host desktop, clipboard, file-share, credential, or
default network bridge** as the arbitrary-GUI isolation boundary. On macOS, the first proof should
use Apple's Virtualization framework, an immutable measured Linux base image, a per-Run
copy-on-write disk, one virtual display, and an authenticated guest agent over virtio socket.

GrokPtah renders the VM display inside its own Computer Run surface. Agent pointer/key/text events
are messages to the guest agent and become input only inside the guest. They are never translated
to `CGEventPost`, Accessibility activation, AppleScript, clipboard writes, or coordinates on the
host desktop. The user's OS pointer remains a separate object. The agent cursor is a GrokPtah
overlay bound to the exact frame and action state.

The local macOS SDK independently confirms the required platform primitives and boundary:

- `VZVirtualMachineConfiguration` and `VZVirtualMachine` require the owning **helper** to carry the
  `com.apple.security.virtualization` entitlement; the main GrokPtah process should not receive that
  authority merely to display status or control the helper;
- `VZVirtioGraphicsDeviceConfiguration` supplies a display for `VZVirtualMachineView`;
- `VZVirtioSocketDeviceConfiguration` supplies host/guest socket communication;
- screen-coordinate pointing devices exist for VM input, but the product path will not forward the
  host pointer. Agent input enters through the authenticated guest protocol instead;
- bridged networking requires a separate `com.apple.vm.networking` entitlement. The isolated
  profile does not request or configure a bridged network device.

Exact signing, notarization, App Sandbox, helper-process, and distribution requirements must be
revalidated against the SDK used for the release build. Header availability is not packaged proof.

## Why the other candidates do not close Stage 9

| Candidate | Disposition | Reason |
|---|---|---|
| Hidden/offscreen host window | Rejected | Same WindowServer, focus, Accessibility, clipboard, and host input domain. Hidden is not isolated. |
| Separate macOS Space | Rejected | The user and agent still share global focus, pointer, keyboard, clipboard, permissions, and WindowServer. |
| `CGEvent` routing or ordinary coordinate injection | Rejected | Mutates the real input domain and can target unrelated windows after focus or geometry drift. |
| Background Accessibility | Separate Stage 8 tier | Useful only for explicitly measured semantic actions. It is neither visual fallback nor an isolated pointer/key domain. |
| In-process deterministic simulator | Test fixture only | Proves policy and replay logic, not process, display, guest, cleanup, or packaging isolation. |
| Sandboxed iframe/WebView | Supported-target follow-up | A valuable isolated browser surface, but it cannot satisfy arbitrary native GUI fallback by itself. It may later use the same frame/input protocol. |
| Remote desktop service | Rejected as product dependency | Violates #288's explicit non-goal and creates a new trust, credential, availability, and data-egress boundary. |
| Disposable VM | Selected | Own display/input domain, bounded lifetime, independently stoppable, compatible with arbitrary certified guest GUI applications. |

## Trust and lifecycle boundary

The host creates one `surface_id` and random `incarnation` for one Computer Run. It also creates a
random in-memory channel secret and a fresh copy-on-write disk whose path is never sent to the
model, guest, public projection, audit log, or MCP client. The immutable base-image digest,
packaged helper identity, guest-agent protocol version, and VM configuration digest are measured
before the Run can receive an isolated proof.

```text
local operator approval
        |
        v
GrokPtah host policy/store ── exact Run + surface incarnation + frame epoch
        |
        | authenticated bounded virtio-socket messages
        v
disposable guest agent ── guest compositor / virtual HID / application
        |
        v
one virtual display ── bounded frame ── GrokPtah surface + agent cursor overlay
```

No network device, shared directory, host clipboard integration, USB passthrough, camera,
microphone, credential forwarding, host home mount, or host application enumeration is present in
the default isolated profile. Adding any one of those is a new capability and security review; it
cannot be enabled by a model prompt or provider profile.

The guest starts from a read-only measured base plus an empty per-Run overlay. Stop, cancel,
takeover, timeout, disconnect, helper failure, guest crash, app crash, or host restart performs the
same terminal sequence:

1. revoke grants and invalidate the surface incarnation;
2. close the authenticated channel and stop accepting frames/events;
3. request VM stop through the out-of-band host handle;
4. force-terminate the packaged helper after a bounded grace period;
5. verify no helper/VM process or open handle owns the exact overlay;
6. remove that exact overlay and bounded frame cache;
7. persist an `interrupted` or `cancelled` audit result without automatic resume.

An uncertain guest input poisons only that surface incarnation. It is never replayed after a
timeout or reconnect. Another independently measured surface may continue.

## Closed protocol

Every message carries protocol version, Run ID, surface ID, incarnation, monotonic frame sequence,
monotonic input sequence, request nonce, bounded payload length, and an authenticator derived from
the in-memory channel secret. Unknown fields, duplicate/non-monotonic sequence, stale frame,
wrong incarnation, malformed coordinates, oversized text, unsupported key/button, or wrong
postcondition fail closed.

Host to guest messages are restricted to:

- `observe`: request one bounded display frame and redacted guest-only accessibility summary;
- `pointer_move`: move the **guest** pointer within the exact virtual display;
- `pointer_button`: press/release one allowed guest button, enabling an auditable drag state;
- `scroll`: bounded guest scroll deltas;
- `key`: bounded guest key down/up for an allowlisted non-credential key set;
- `text`: bounded Unicode text input through the guest input method, never clipboard paste;
- `stop`: terminate the guest agent and acknowledge shutdown.

Guest to host messages are restricted to bounded frame metadata/bytes, guest cursor position,
redacted semantic hints, input acknowledgements, postconditions, health, and shutdown. They cannot
contain a host path, environment value, credential, arbitrary log stream, shell output, or request
for a broader capability.

The Stage 9 contract candidate closes the type-shape gap without enabling a backend. In addition to
the compatibility `PointerClick`, `ComputerAction` now has isolated-only `PointerMove`, explicit
`PointerButton { state: Down | Up }`, and `TextInput` variants. This makes drag state auditable and
keeps guest character input distinct from semantic `SetValue` and clipboard paste. Policy binds
pointer coordinates to the current observation geometry and admits all three variants only for an
independently isolated, dispatchable proof. Foreground and measured-background backends, legacy
serialized grants, the macOS Accessibility adapter, the agent proposal schema, and the cockpit
approval path continue to reject them. Only the simulator-only isolation fixture can exercise the
contract until the packaged VM proof exists.

## Bounded resources

Initial hard ceilings for the proof, all lowerable by local policy:

- one VM per physical host until capacity and pressure tests prove more;
- 2 vCPU, 4 GiB guest RAM, 8 GiB copy-on-write overlay;
- one 1280×800 display at at most 10 encoded frames/second;
- 16 MiB encoded frame maximum and the existing cumulative evidence bound;
- 600-second default and 30-minute absolute Run duration;
- 256 input events and 4 KiB per text event;
- no automatic restart or resume;
- exact overlay/helper ownership recorded before launch and cleanup.

Exhaustion returns a typed `limit_reached` result and destroys the surface. It does not evict an
unrelated Run or fall back to the live desktop.

## Proof campaign

The first disposable guest image contains only a multi-step visual fixture and the minimal signed
guest agent. The fixture must require pointer movement, click, Unicode text, keyboard navigation,
scroll, drag, and final visual confirmation. The campaign records:

1. exact host/build/helper/base-image/configuration digests;
2. host foreground application, active window, physical pointer, and clipboard digest before and
   after without storing clipboard content;
3. every frame digest, input sequence, guest cursor coordinate, acknowledgement, and postcondition;
4. an independent guest-side event trace;
5. negative cases for stale frame, duplicate event, surface loss, channel authentication failure,
   guest crash, helper kill, timeout, takeover, app restart, and cleanup failure;
6. exact process/open-handle and disk-usage checks before deleting the disposable overlay;
7. a packaged wide/narrow/light/dark cockpit capture with visible agent cursor, focus/drag preview,
   activity, timeline, Stop, and Take over states.

The proof fails if the host pointer, foreground application, active window, or clipboard digest
changes; any unrelated host window is visible; any host path or secret enters evidence; an input is
duplicated or resumed; cleanup is incomplete; or a missing entitlement/image/helper is shown as
available.

## Implementation order

1. Extend the typed isolated-only input contract; keep host-native isolated proof non-dispatchable.
   The pointer move/button and text-input type/policy slice is implemented in the Stage 9 candidate;
   authenticated transport events and replay evidence remain part of steps 4–5.
2. Build a read-only Virtualization-framework availability/configuration probe with no VM launch.
   The Stage 9 probe candidate now checks the minimum OS and required framework classes, then
   reports the first blocker in Settings. It hard-codes the helper entitlement/image as unverified,
   records `launchAttempted: false`, and does not give the main app the helper's virtualization
   authority; it cannot mint a capability proof.
3. Package and measure a minimal helper and immutable guest fixture; run the no-input lifecycle and
   cleanup campaign. The candidate now defines the closed manifest/security/resource contract and
   a deterministic no-input lifecycle that refuses terminal completion until exact process,
   open-handle, overlay, and frame-cache cleanup evidence matches the bound surface. It contains no
   host paths or channel secrets. A subsequent measurement candidate hashes independently opened,
   read-only regular-file handles under helper/image/configuration size and mode ceilings, preserves
   their offsets, detects identity changes during streaming, emits no paths/descriptors, and can
   compare those three content digests with the launch manifest. It does **not** discover bundle
   resources, reject a symlink at open time, establish the helper's code-signing requirement,
   package an artifact, launch a VM, or run cleanup. Actual helper/image packaging, signing proof,
   and the destructive campaign remain.
4. Add authenticated virtio-socket frame/health transport and render read-only frames. The candidate
   now defines the transport-independent, read-only protocol core: a non-serializable/redacted
   32-byte channel key authenticates the exact protocol version, Run, surface incarnation, message
   and frame sequences, zero input sequence, one outstanding request nonce, encoded payload length,
   and closed observe/frame-metadata/health/failure/stop/shutdown-ack payload. Tamper, replay,
   wrong-secret, wrong-nonce, input, oversized-frame, and unknown-field paths fail closed. No carrier,
   frame-byte transfer, guest agent, or renderer exists yet, so this is not transport or isolation
   proof.
5. Add guest pointer state and one-action local approval; then key/text/scroll/drag independently.
6. Integrate app-owned cursor, focus/drag preview, timeline, persistent emergency controls, and
   accessibility states in the cockpit.
7. Run adversarial, crash/restart, resource, packaged hardware, and recurring expert UI reviews.
8. Enable `HostNative` isolated dispatch only for the exact packaged backend ID and measured
   configuration after independent security review. Unknown helpers and serialized claims remain
   unproven.

## Status and nonclaims

The existing simulator remains the only dispatchable isolated proof. The Stage 8 measured
background candidate is not a substitute. The typed input, read-only host-probe, no-input lifecycle,
open-handle content-measurement, and authenticated read-protocol candidates do not enable
`HostNative`, expose isolated actions to a model or cockpit approval flow, qualify a provider for
visual fallback, package a VM image/helper, establish code-signing identity, carry or render frame
bytes, or satisfy any #288 acceptance checkbox. The manifest accepts only the locked-down profile
and bounded resources, but synthetic or caller-opened content digests are not packaged identity. A
present framework, valid contract, content hash, authenticated metadata envelope, or entitlement is
not a VM, signed helper, guest, carrier, rendered frame, isolation, dispatch, cleanup campaign, or
release proof.
