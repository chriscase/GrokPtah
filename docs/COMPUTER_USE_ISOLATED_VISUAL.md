# Isolated Visual Computer Use

> **Reconstruction status.** This substrate was reconstructed onto the exact head of PR #424,
> `6c1c4c3cd8d0398f1d673a04d6187c6e60780780`, from the read-only donors recorded in
> [`COMPUTER_USE_ISOLATED_VISUAL_DONOR_MATRIX.md`](COMPUTER_USE_ISOLATED_VISUAL_DONOR_MATRIX.md).
> Every runtime module is private to `computer_use`, nothing is re-exported from the crate root,
> and no dispatch path reaches any of it. `computer_isolated_visual_status` reports
> `dispatchEnabled: false` and is not configurable.
>
> Nothing here has been measured on real hardware. No guest has been booted, no helper spawned, no
> package signed or measured, and no qualification, canary, or soak result is claimed by this
> branch. The measured gates and the independent security and accessibility reviews described below
> are still outstanding, and they are what would change that.


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

Capability admission has a separate proof fence as well: the ordinary
`grokptah.computer-qualification.v1` record measures semantic observation/action against the
in-process fixture and can never grant visual fallback. A model is downgraded to semantic
authority unless its exact **measured** capability record carries
`grokptah.isolated-visual-computer-qualification.v1`, which may be written only after the
credentialed packaged-runtime campaign and independent review pass. Until then, a declared or
manually supplied visual tier does not expose isolated visual authority.
The separate schema is preserved only in the route-bound, host-managed qualification store;
user-editable compatible-provider profiles are normalized back to no measured visual authority.

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

The credentialed handoff procedure is deliberately **not** carried by this reconstruction: it
belongs with the signing and hardware custody that this branch does not have. The property it
protects still holds here and is enforced in code rather than by procedure — source, image, and
package checks cannot be promoted to a VM capability result without the complete signed-runtime
campaign, and nothing in this branch can write a measured visual capability record.

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
   compare those three content digests with the launch manifest. The latest source candidate adds a
   fixed package verifier for the exact app, helper, guest-image, and configuration paths. It opens
   each artifact read-only with no-follow semantics, retains the handles, strictly validates the
   entire app including nested code, separately validates the helper, requires matching non-ad-hoc
   hardened signing identities, and hashes the helper's canonical designated requirement. The main
   app must have no virtualization authority; the helper must be sandboxed and may carry only its
   exact application/team identity, App Sandbox, and virtualization entitlements. VM networking,
   debug attachment, mismatched teams, unreviewed helper entitlements, path replacement, and
   content/manifest drift fail closed. This is verifier **source**, not evidence that the helper,
   guest, configuration, signing pipeline, or entitlement profile is actually packaged. The next
   source slice adds a minimal helper, its exact App Sandbox + Virtualization entitlement file, a
   closed configuration, and a credentialed assembler. The helper accepts only inherited immutable
   guest/configuration handles plus private control/event pipes; it clears its environment, refuses
   arguments, builds a bounded one-display/virtio-socket VM with no network/share/audio/storage/host
   input devices, requires an explicit start byte, performs a challenge/response ready handshake
   with the guest bootstrap agent, and requires an authenticated shutdown acknowledgement before
   bounded graceful-then-forced stop.
   The helper's fixed event/control ABI is shared with the freestanding protocol header, and a
   host-supervisor codec/state machine rejects reordered, unknown, and post-terminal events. The
   candidate now also contains a macOS packaged-supervisor seam: it consumes the already-measured
   helper/guest/configuration descriptors, rechecks the helper path/signature immediately before
   `posix_spawn`, maps only the fixed guest/configuration and five private channel descriptors under
   `POSIX_SPAWN_CLOEXEC_DEFAULT`, consumes the private per-launch challenge, and owns bounded helper
   lifecycle/cleanup. This remains source-level wiring only:
   no signed package has launched it and no VM boot/render/input/cleanup evidence exists.
   The assembler signs the helper before the outer unprivileged app and derives the content and
   designated-requirement manifest. CI links only the unsigned helper source. The repository now
   also carries a pinned Linux arm64 guest-source lock, closed kernel fragment, freestanding guest
   PID 1, protocol self-test, and a Linux-only deterministic image-builder candidate. Dedicated
   Linux CI builds that source twice and compares image/manifest bytes, but no output is embedded
   or reviewed as a release artifact. This slice supplies no reviewed guest image, signing
   identity, built helper, assembled app, packaged launch, or cleanup run. Actual guest production
   plus signed-package and destructive campaign evidence remain.
4. Add authenticated virtio-socket frame/health transport and render read-only frames. The candidate
   now defines the transport-independent, read-only protocol core: a non-serializable/redacted
   32-byte channel key authenticates the exact protocol version, Run, surface incarnation, message
   and frame sequences, zero input sequence, one outstanding request nonce, encoded payload length,
   and closed observe/frame-metadata/health/failure/stop/shutdown-ack payload. It also defines a
   separate authenticated binary guest-to-host frame-chunk carrier with bounded 64 KiB chunks,
   whole-frame SHA-256, exact offsets, and strict reassembly. Tamper, replay, wrong-secret,
   wrong-nonce, input, oversized-frame, reordered-chunk, digest-mismatch, and unknown-field paths
   fail closed. The helper/guest bootstrap handshake and bounded FD7/FD8 relay are now present;
   the freestanding guest has a fixed `/dev/fb0` capture source, a deterministic fixture renderer,
   and authenticated frame-chunk emitter; validated fixture pointer/button/scroll state changes
   the next surface, but no reviewed GUI guest image or host renderer is wired into a packaged
   runtime yet. The helper returns its per-launch challenge over a private inherited channel so the
   supervisor can bind the host and guest without exposing challenge material to a model; the
   supervisor source candidate reads that channel but has not been run from a signed package.
5. Add guest pointer state and one-action local approval; then key/text/scroll/drag independently.
   The source candidate now includes a non-dispatchable host-side input gate that binds every
   pointer/button/scroll/key/text edge to the latest frame, requires strictly increasing input
   sequence, enforces the measured display/text/event ceilings, and refuses terminal state while a
   key or button remains held. The host-side gate does not itself authenticate or send a socket
   message; the packaged guest agent and one-action approval path remain required before any input
   claim. A separate
   authenticated binary host-to-guest input packet ABI now binds the same frame/input sequences and
   exact Run/surface/incarnation, but it is source-only and not dispatched. A shared session-binding
   contract now length-prefixes Run, surface, incarnation, and input-domain identities, derives a
   challenge-bound channel key, and carries a confirmation tag before frame/input traffic. Rust and
   freestanding guest C share the digest and vectors; the Rust frame/input carriers can derive
   interoperable keys from the same challenge, and the helper/guest source loop now consumes the
   binding packet and returns an authenticated acknowledgement. The host-supervisor source state machine now refuses terminal stop
   until it has sealed the binding packet, preserving the intended lifecycle ordering without
   claiming that a packaged process performs the exchange. The freestanding guest now accepts the
   binding command and returns an authenticated binding acknowledgement; the existing zero-binding
   STOP path remains available for the current bootstrap smoke until the supervisor is wired. The
   helper source now defines the corresponding private control-channel relay and validates the
   guest acknowledgement. The packaged-supervisor source candidate invokes the relay through
   inherited descriptors, but no signed package has exercised that path yet. A source-only
   `IsolatedVisualRuntimeSession` now couples that helper event order to lifecycle cleanup, frame
   freshness, and challenge-bound input admission; it still does not spawn or dispatch a packaged
   runtime. A bounded length-delimited `IsolatedVisualStream` now supplies the private transport
   seam, delegating frame authentication and input admission to that coordinator; it still does
   not open a VSOCK itself. `IsolatedVisualHelperControl` similarly binds inherited helper
   control/event descriptors to the coordinator. `IsolatedVisualPackagedRuntime` now owns the
   native child process and descriptor transfer on macOS, while a source-only
   `IsolatedVisualRuntimeDriver` joins those helper and stream adapters behind one
   lifecycle-owned API, preventing a future supervisor from advancing them through unrelated
   state machines. The
   freestanding guest now validates the authenticated length-bounded input ABI after binding, while
   still refusing input until a real captured frame establishes a freshness fence. It also
   independently tracks held mouse-button and key state, rejecting duplicate downs, mismatched
   releases, and shutdown while input remains held. The exact source candidate is the guest tree in this
   repository; no prior evidence record or digest set is carried forward, because none of it was
   measured on this branch.
6. Integrate app-owned cursor, focus/drag preview, timeline, persistent emergency controls, and
   accessibility states in the cockpit.
7. Run adversarial, crash/restart, resource, packaged hardware, and recurring expert UI reviews.
8. Enable `HostNative` isolated dispatch only for the exact packaged backend ID and measured
   configuration after independent security review. Unknown helpers and serialized claims remain
   unproven.

## Qualification handoff

This is the standing answer to "what would it take to call the packaged path qualified?". It is
scoped to the packaged helper/guest lane. Provider execution authority, Semantic Help, and the
semantic macOS adapter are deliberately out of scope here.

Everything in the **Proven now** column is deterministic and provider-free: it runs on any host
with `cargo test`, opens no VM, helper, socket, or package, and needs no credentials. None of it
certifies real hardware. A scripted adapter yields whatever it was scripted with, and the measured
harness refuses before it reaches a guest, so no column below may be read as a launch result.

| Gate | Proven now (provider-free) | Remaining to qualify |
|---|---|---|
| Signed packaging | Shipped profile and limits deserialize into exactly the locked-down Rust contract; the kernel fragment compiles out network, storage, USB, and audio bridges rather than leaving them unconfigured; the helper declares only sandbox and virtualization entitlements and the main app declares none; guest source is pinned to a TLS URL and lowercase SHA-256 (`isolated_visual_package`) | A Developer ID-signed, notarized build, and the packaged verifier run against that real bundle. Source tests are not runtime evidence. A reproducible guest image built twice on a clean runner, published with its digest |
| Real guest boot | Launch-stage ordering, fail-closed refusal, and the never-retry-an-uncertain-step rule over `MeasuredLaunchSteps`; the real harness refuses without an operator opt-in and a measured signed package (`isolated_visual_harness`) | An actual Virtualization.framework boot on Apple silicon reaching `GuestBooted`, with `Prepared` and `Running` observed over the private descriptor topology |
| Rendered frames | Authenticated frame sealing, chunked open, freshness fencing, and that frame bytes reach neither a report nor a projection (`isolated_visual_frames`, `project_captured_artifact`) | A real virtio-gpu frame rendered by a booted guest and opened by the host: at least four frames, each strictly fresher and visibly different from the last |
| Host input | Input-wire sealing with sequence and nonce binding, refusal before guest binding, and the rule that an acknowledgement is not a postcondition — the next frame is (`isolated_visual_input_wire`, `isolated_visual_harness`) | Pointer, keyboard, and Unicode events delivered to a real guest, each with a visible postcondition in the following frame |
| Live cleanup | Terminal cleanup requires all five facts — guest stopped, helper absent, no open handles, overlay removed, frame cache removed — and the guest-stopped fact is derived from observed helper state rather than accepted from the caller (`isolated_visual_cleanup_gates`) | A live run whose overlay, frame cache, open handles, and helper process are checked against the OS after stop, including the crash, kill, and restart paths |
| Soak | The source canary repeats the contract rehearsal identically across iterations; a hardware soak is structurally unstartable because `HardwareGateEvidence` cannot be constructed without a measured launch (`isolated_visual_soak`) | A bounded repeated real-guest campaign, which cannot begin until a measured launch exists and both independent reviews have passed |
| Accessibility | Nothing. The isolated surface is not dispatchable and has no cockpit surface on this branch | An independent accessibility review of the agent-cursor, focus, drag preview, activity, timeline, Stop, and Take over states, including keyboard-only operation and reduced-motion behaviour, before the tier is offered |
| Licensing | The guest source is pinned to an exact upstream release and digest | License review and attribution for the guest kernel and any guest-side agent shipped inside the image, recorded in `THIRD-PARTY-NOTICES`, plus confirmation that redistributing the built image meets its terms |

### The guest-stopped fact

Terminating a run is the strongest statement this lifecycle makes: it asserts that no guest,
helper, handle, overlay, or frame cache survives. Four of those are host-observed resource checks.
The fifth — *the guest itself stopped* — is now recorded as its own fact and **derived**, because
writing the stop control byte is not the same as the helper acknowledging it.

`IsolatedVisualRuntimeSession::complete_observed_cleanup` therefore does not take the guest-stopped
fact as an argument. It reads the helper state the session actually observed:

- a clean acknowledged stop, a helper report that the guest stopped, or a failure raised before any
  guest could exist proves the guest is not running;
- a control byte written but never acknowledged (`StartSent`, `StopSent`), a live guest (`Running`,
  `BindingSent`, `Bound`), and a failure that can leave a guest behind (`StartFailed`,
  `ControlLost`, `StopFailed`, `GuestProtocol`) prove nothing and fail cleanup closed.

A run interrupted by restart is the single exception, and only together with observed helper-process
absence: the helper owns the guest, so a helper that is gone across a restart leaves none behind,
and the lifecycle already records `Interrupted` rather than a clean stop. On macOS the supervisor
owns the helper PID and its reap state, so a caller's process-absence claim is intersected with what
that supervisor observed; an unreaped helper fails cleanup closed.

### Verification command

```sh
cargo test --locked --manifest-path crates/codegen/grokptah-agent-bridge/Cargo.toml \
  --lib computer_use:: -- --test-threads=1
```

## Status and nonclaims

The existing simulator remains the only dispatchable isolated proof. The Stage 8 measured
background candidate is not a substitute. The typed input, read-only host-probe, no-input lifecycle,
open-handle content-measurement, fixed-path packaged-identity verifier, unshipped helper/assembler
source, pinned guest-source/image-builder candidate, authenticated read-protocol candidates, and
packaged-supervisor source seam do not enable
`HostNative`, expose isolated actions to a model or cockpit approval flow, qualify a provider for
visual fallback, package a VM image/helper, carry or render frame bytes, or satisfy any #288
acceptance checkbox. The verifier can establish signed package identity only when invoked by a real
correctly signed package containing the exact artifacts; source tests, Objective-C syntax checks,
and an unsigned helper link are not that runtime evidence. The manifest accepts only the locked-down
profile and bounded resources, but synthetic or caller-opened content digests are not packaged
identity. A present
framework, valid contract, verifier implementation, content hash, authenticated metadata envelope,
or entitlement declaration is not a packaged helper, booted guest, carrier, rendered frame,
isolation campaign, dispatch, cleanup campaign, or release proof.
