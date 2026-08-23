# Computer Use

Computer Use is tracked by epic [#267](https://github.com/chriscase/GrokPtah/issues/267).
It is intentionally staged. The safety kernel, simulator, desktop operator cockpit, native macOS
observation, and the first semantic macOS action slice share one bounded run contract. A selected,
qualified model can now propose one semantic action at a time, but only the local cockpit can stage
and approve it. Computer Use is not exposed as an MCP mutation surface.

## Safety boundary

Computer Use treats observation and action as separate privileged operations:

1. A local user selects an exact application/window target.
2. GrokPtah creates a bounded run in `awaiting_authorization`.
3. A local-user grant binds that run, target generation, allowed action classes, expiry, and
   optional remaining-use count.
4. An observation receives a monotonic ID. Every action must reference the current observation.
5. Policy is checked again immediately before the backend action. Takeover is durable
   bookkeeping-safe (revokes grants, bumps epochs, cancels later work). The stacked emergency
   control candidate removes the UI mutation-busy gate and stale client-version gate from Pause,
   Stop, and Take over; stable keyboard paths remain available while a backend call is unresolved.
   The stacked native-cancellation successor gives each macOS action an exact Run-scoped monotonic
   signal: emergency control no longer waits behind the action mutex, and native preflight plus the
   bounded activation loop poll that signal before dispatch. An Accessibility call that has already
   entered one atomic OS API cannot be undone; cancellation at or after that boundary remains uncertain.
6. Successful actions invalidate the observation, forcing the caller to observe again.

Authorization is fail-closed. Grants do not survive restart, pause, cancellation, completion,
failure, target changes, or exhausted limits. Secure and system-restricted surfaces are denied
even when a grant exists. Pointer fallback and key chords require a typed independently isolated
visual input-domain proof; capability booleans, a blanket grant class, and a simulator fixture
cannot make a native backend isolated. A semantic-action grant cannot silently expand into raw
input control.

## Isolation contract (stage 1)

Surface isolation, surface incarnation, initiating principal, authority epoch, frame/observation
generation, and isolation proof are intrinsic to the Computer Use contract. Backends advertise a
closed `ComputerCapabilityTier`:

- `foreground_semantic` — may require activating the real target. Current macOS native Computer
  Use is this tier and must not be advertised as isolated. It is one host-global-foreground
  conflict domain (capacity 1). Per-window IDs are target identity, not isolation.
- `measured_background_safe_semantic` — host-measured semantic actions that must not activate or
  move the pointer. `ActivateTarget` is forbidden and cannot silently fall back to foreground.
- `independently_isolated_visual_input_domain` — pointer, key, and visual actions. Stage 1 only
  the deterministic simulator may fixture this origin; a simulator fixture cannot stamp a native
  backend isolated. Host-native isolated helpers are a later stage and fail closed.
- `unproven` — missing, unknown, legacy, or contradictory capability. Fail closed.

`ComputerCapabilityProof` is the security boundary. Public booleans on `ComputerCapabilities`
are a derived projection. Legacy boolean-only records hydrate to foreground-semantic when they
claim only observe/semantic/text entry, or stay unproven when they claim pointer or key
authority. Unknown, malformed, host-native isolated, or contradictory tier/proof/boolean
combinations cannot deserialize into background or isolated authority.

Every run, grant, observation, and policy check is bound to a host-interned opaque surface ID and
incarnation for an attested physical input domain, plus target generation, a host-minted
frame epoch, and control/authority epoch. Native macOS interns one host-global-foreground domain;
two windows share that domain's freshness clocks. Backends do not mint surface authority; syntactic
prefixes are not proof of issuance. A serialized wall-clock timestamp is not dispatch proof.
Monotonic freshness ticks are exact-current for the live surface incarnation: an older tick is
stale. Restart invalidates the live clocks.

Local-operator caller identity is a host-resolved `ComputerAuthorityToken`. Public principal,
proof, surface, and grant constructors do not mint authority. Agent authority is issued only by
`AgentHost` after it resolves an active durable `AgentRecord`, its exact current `AgentSpec`
revision with `computer_use_allowed=true`, an assigned Work Item, and the exact live WorkAttempt
claimant. The public `ComputerPrincipal::agent` constructor remains fail-closed, and an Agent-shaped serialized
principal is not an authority token.

Unproven capability fails closed for observe, evidence, grant, and act. Missing initiating
principal is not treated as the local operator. Idempotency receipts are bound to the immutable
caller principal and run authority/control epochs; replay reauthorizes before returning typed
data. Legacy unstamped receipts fail closed.

`ActivateTarget` is valid only for explicitly authorized foreground-semantic execution. It is
never non-disruptive. Restart/reopen rotates the surface incarnation, zeros the freshness tick,
bumps the authority epoch, clears grants and observations, and coerces isolated or background
proofs to unproven. A second reopen of an already-interrupted run is idempotent.

Public projections expose `capabilityTier`, opaque `surfaceId` / `surfaceIncarnation`,
`authorityEpoch`, and `initiatingPrincipalKind` only. They never include native process or
window handles, raw attestation material, agent IDs, spec revisions, input-domain IDs, or
measurement IDs.

### Measured-background text entry candidate

The Stage 8 candidate implements one deliberately narrow native instance of
`measured_background_safe_semantic`: visible Accessibility `set value` on one exact, locally
calibrated text field. It is not inferred from an application name, bundle allowlist, AX action
name, hidden window, or successful foreground action.

Calibration requires the local operator to select an exact visible, non-minimized background
window, name one unambiguous non-secure text field, and acknowledge that the target is disposable.
GrokPtah changes that field to a distinct probe value, measures the frontmost process, active
foreground window, and physical pointer before and after native dispatch, verifies the AX
postcondition, restores the original value through the same measured path, and re-observes the
restored value. Any ambiguous label, unreadable value, secure/sensitive field, truncated tree,
foreground transition, pointer movement, active-window change, restore failure, or uncertain
native outcome fails closed.

Success returns an opaque, two-minute, one-use receipt bound to the exact picker token, target
generation, native process/window identity, and semantic-element digest. Consuming it creates a
run whose proof and grant allow `text_entry` only. Every real action repeats exact target/tree
freshness checks plus native foreground-app, active-window, pointer, and postcondition measurement.
There is no activation, raw input, clipboard, cross-application switch, or silent foreground
fallback. `invoke`, `select`, `scroll`, and arbitrary targets remain unsupported at this tier until
they receive their own measured calibration and evidence.

This is a source-qualified candidate, not a claim that [#287](https://github.com/chriscase/GrokPtah/issues/287)
or Stage 8 is complete. Packaged native disposable-fixture evidence, strict Rust/native
qualification, and the remaining per-action/target dispositions are still required.

This stage does **not** implement an isolated helper process, visual compositor, agent cursor
UI, general background Accessibility execution, pointer/keyboard injection on the real desktop,
or physically reversible takeover after an atomic OS API begins. Those remain later stages; this contract makes them
structurally representable without lying that macOS is isolated today. The candidate now applies
role-scoped bearer authority and immutable session/workspace grants to MCP Computer reads;
packaged hardware/TCC/takeover remain fail-closed or unverified. The host now does implement the
durable WorkAttempt surface-lease and physical-dispatch coordinator described below, without
exposing raw leases or widening MCP mutations.

## Inter-Agent surface coordination

An Agent-owned Computer Run is admitted only through `AgentHost::create_agent_computer_run`.
The public request contains a Work ID and WorkAttempt ID, but cannot supply a session, workspace,
Agent ID, spec revision, surface, conflict domain, queue sequence, or authority epoch. The host
resolves and freezes all of those values from the durable orchestration ledger.

Before Agent authorization, observation queueing, dispatch preparation, and the irreversible
input boundary, the service revalidates the exact Work, active unexpired WorkAttempt, claimant,
assigned Agent, current AgentSpec revision, the AgentSpec's explicit Computer Use policy, owning
Lane, and workspace. The final check and
durable dispatch transition are linearized while the Work ledger is locked. Work cancellation,
lease expiry, reassignment, or Agent-spec revision therefore revokes the old Computer authority;
it cannot silently continue under a stale token.

One host-attested physical input conflict domain owns at most one granted or dispatching lease.
Agents sharing the native macOS foreground domain queue in deterministic FIFO order. A normal
waiter ages so future priority classes cannot starve it. Operator Take over, pause, and cancel
use the same store linearization fence as Agent injection. Independently attested simulator
domains may proceed concurrently; separate window IDs on the native desktop do not create
separate domains.

Every physical action has a stable durable dispatch ID and a closed transition:

```text
queued -> granted -> prepared -> injected -> acknowledged
                    |            |
                    |            +-> uncertain (never replay automatically)
                    +-> known_not_injected (safe terminal result)
```

Restart invalidates queued/granted leases, converts prepared dispatches to
`known_not_injected`, and converts injected dispatches to `uncertain`. Reopening the same store a
second time is a no-op. Expiry uses the same distinction. Corrupt or future-shaped lease records
fail store open before recovery can rewrite Runs. The coordinator remains internal: the current
product has no MCP Computer mutation surface or agent-owned cursor UI. The stacked local queue
explanation below remains unqualified until its Rust, desktop, and packaged-UI gates pass.

The coordination ledger is bounded to 512 lease records. `released`, `revoked`, `cancelled`, and
`quarantined` records age out after seven days and are retired oldest-first when a new admission
needs space; during its declared retention horizon, the separately persisted mutation receipt
remains the exact-request replay fence.
`uncertain` physical dispatches are never deleted to regain capacity. If unresolved uncertainty
fills the ledger, new Computer Use fails closed with `limit_reached` until an operator-facing
reconciliation clears exact uncertain records. This prevents ordinary long-running use from
exhausting the coordinator without converting a storage bound into permission to replay an ambiguous action.
An unresolved `uncertain` dispatch also poisons its exact physical input conflict domain: no other
Agent may observe or act through that domain until reconciliation. Independently attested isolated
domains remain available, so one ambiguous isolated surface does not stop unrelated isolated work.

The stacked cockpit candidate derives a separate, local-operator-only projection from that ledger.
It shows the selected Run's queued/granted/dispatching/uncertain state, its deterministic queue
position, total live waiters, and the stable Agent/Work/Run identity currently holding capacity.
Lease IDs, revisions, dispatch IDs, WorkAttempt IDs, conflict-domain IDs, frame epochs, and
authority handles never enter the DTO. Workspace-scoped coordinator reads do not receive host-wide
queue depth or another workspace's Agent identity. This makes contention understandable without
turning the read surface into a cross-workspace activity oracle. It is not an agent-owned cursor,
an out-of-band preemption channel, a background-safe backend, or isolated visual execution.

The stacked emergency-control candidate treats Pause and Take over like Stop: each local gesture
gets a fresh mutation request, authorization is re-evaluated against the current durable Run while
the store is locked, and no client-held Run version can make the control lose to an in-flight
action. The cockpit does not disable Stop or Take over behind its ordinary mutation busy flag and
advertises `Control+Shift+S` / `Control+Shift+T` keyboard paths. These changes close the known
surface reachability and stale-version race only. The native backend still needs a genuinely
out-of-band cancellation channel before takeover is physically preemptive. The stacked native-
cancellation successor supplies that channel for work waiting in Rust/native preflight and for the
bounded activation wait: a C11-atomic per-action signal is tripped immediately, only for the exact
Run, and without taking the action gate. It is checked before Accessibility dispatch and again
before accepting completion.
This closes the mutex inversion; it does not pretend that macOS can roll back a synchronous AX call
after the call has entered the operating system. That narrow boundary stays `uncertain` and must
never be replayed automatically.

The app-owned surface candidate binds every open cockpit and shell control to an exact durable
Run ID. An unbound lookup is used only to discover a unique non-terminal Run and fails closed when
more than one exists; one-shot observation previews are never eligible. Pending approvals are
stored per Run, so reading or controlling another Lane cannot clear them. While a Run remains
live, the application shell polls that exact binding and keeps Pause, Take over, and Stop visible
after the cockpit closes, the user focuses another Lane, or the owning Lane is archived. Owners
receive an opaque host-issued emergency-control token that can enter only those revoking service methods; ordinary
observation, grant, proposal, approval, and action authority still requires the foreground Lane.
This is persistent control-plane reachability, not proof of native physical preemption.

The stacked typed-replay successor gives every durable audit row a closed,
redaction-safe `ComputerSurfaceEvent` value and exposes cursor replay only for
the exact app-owned session/Run binding. The application shell owns that replay
rather than the cockpit panel, persists the exact cursor across reloads, and
keeps a detected retention gap sticky for the lifetime of the Run. After an
expired cursor it resumes immediately before the retained window so current
events can remain visible, but it never relabels the retained tail as a complete
history. Both the persistent emergency strip and the cockpit show the gap while
leaving Pause, Take over, and Stop usable.

The stacked agent-attention successor records `action_proposed`, optional
`attention_moved`, `approval_required`, and `approval_rejected` as typed durable
surface events. Attention coordinates are normalized to `0..=10_000` basis
points inside the authorized observation surface. They never retain a screen
origin, display layout, native handle, semantic element ID, observed label or
value, typed text, or model prose. A semantic element contributes a point only
when its complete bounds are inside the current observation; missing or
out-of-surface geometry produces no positional marker. Manual operator proposals never
produce agent-attention evidence.

The cockpit renders that evidence as an app-owned `Agent` marker and accessible
live status, explicitly stating that the operating-system pointer has not
moved. Simulator observations can show the normalized point. Native semantic
observations mark the exact referenced row without fabricating a pointer
location. Approval rejection removes the active marker, while the durable event
remains replayable. This successor still does **not** add native physical
preemption, a background-safe backend, an isolated helper/input domain, or
isolated visual execution. Renderer replay while the host app remains resident
is not evidence that pending approval authority survives a full process crash.

## Foundation (#268)

`grokptah-agent-bridge::computer_use` provides:

- closed, serialized target, observation, semantic element, action, outcome, error, limit,
  grant, audit, and run-state types;
- an explicit state machine with monotonic terminal states;
- a dedicated policy engine for target, grant, sensitivity, freshness, geometry, and action
  checks;
- crash-atomic durable run records and idempotency receipts;
- fail-closed restart recovery: active runs become `interrupted`, authority is cleared, and
  in-flight mutations become `uncertain` rather than being replayed;
- bounded retention and an exclusive store lock;
- a deterministic simulator used to test observation/action behavior without OS access.

Audit entries retain a closed surface-event type plus bounded legacy operation
metadata, dispositions, action classes, observation IDs, optional normalized
attention points, and error codes. Legacy
records without the typed field are classified only in their read projection;
the durable evidence is not rewritten. Audit rows do not retain action payloads,
typed text, screenshots, application values, window titles, credentials, or
arbitrary model content. Attention rows also omit screen origins, element IDs,
labels, values, and native handles. Evidence references are opaque IDs and hashes rather
than filesystem paths.

The durable run projection also exposes `controlDisposition` and `controlEpoch`. A paused run
can be explicitly reauthorized, but `operator_takeover` is an absorbing local-control fence:
stale approvals and reconnects cannot return authority to the agent, and a new Computer Run is
required. Stop records `stopped`; restart recovery records `interrupted`; a late mutation whose
receipt cannot be trusted records `uncertain_outcome` without replaying the action.

## Authoritative run projection (#271 read contract)

`computer_use::projection` derives `ComputerRunProjection`, the single serialized view the
desktop cockpit reads and the one a future coordinator surface will serve. Both surfaces
consuming one projection is what prevents the GUI and an external observer from disagreeing
about who owns a run.

The projection is redaction-safe **by construction**. Observed element roles, labels, values,
and geometry, plus the evidence asset token and content hash, are absent from the type rather
than filtered at a transport boundary, so a coordinator learns that an observation exists, how
many elements it held, whether a screenshot was captured, and whether it is stale — never what
it contained. Element IDs are observation-scoped capabilities and are likewise not projected.
`lastOutcome` and `lastError` follow the same bar: they are dedicated summary types
(`ActionOutcomeSummary` / `ComputerErrorSummary`). Backend-chosen summary text and error
messages stay on the local `ComputerRun` record; the projection carries only
`expectedPostconditionMet` and the closed `code` enum. Restart recovery clears
`last_outcome` so a leaky action summary cannot survive a process restart.

`project_run_at` takes an explicit instant instead of reading an ambient clock. Given the same
`(record, now)`, GUI and coordinator serialize identically — including clock-derived fields
(`progress.elapsedMillis`, `observation.stale`, `grant.expired`). Live MCP calls pass
`Utc::now()` independently, so those three fields are **not** promised byte-identical across
surfaces or across duplicate live calls. Durable fields (state, disposition, epoch, event
range, last-outcome/error summaries, observation metadata other than staleness) do not depend
on the call instant.

The two read gates share the projection type, not an API:

| Surface | Type | Authorization identity |
|---|---|---|
| Local cockpit | `ComputerUseService::{list_session_run_projections, project_session_run, session_run_events, session_capacity}` | Owning session (includes unbound runs) |
| Coordinator / MCP | `ComputerRunReads` taking `ComputerReadBinding` | Session **and** durable workspace binding |

Session-only service methods do not accept `ComputerReadBinding`, so a coordinator
surface cannot be wired to them. An unknown run and a run outside the caller's
authorization identity return the **identical** `unauthorized` error, so a scoped
read cannot be used as a run-existence oracle. Traversal-shaped and empty run ids
fail the same way rather than surfacing a distinct validation error.

### Workspace binding and the MCP read surface

Every Computer Run now carries a **durable workspace binding**: the owning session's
canonical project cwd, stamped at creation, preserved verbatim through restart recovery,
and never rewritten. The authenticated loopback control plane exposes four read-only
tools over this contract — `ptah_list_computer_runs`, `ptah_get_computer_run`,
`ptah_get_computer_run_events`, and `ptah_get_computer_capacity` — each requiring the
owning `session_id` plus the claimed allowlisted workspace, which must equal the run's
binding exactly. A run without a binding (created before the field existed) is invisible
to MCP: authorization fails closed rather than inferring a workspace from current process
state. Unknown session, a mismatched allowlisted workspace, cross-session, unbound,
unknown run, and traversal-shaped reads all collapse into the same
`unauthorized`/`forbidden_scope` failure. The desktop and the
control plane share one `ComputerStore` handle through `AgentHost::ensure_computer_store`
because the ledger holds an exclusive file lock. No mutation, grant, evidence byte, or
screenshot crosses MCP; see `docs/MCP_CONTROL_COORDINATOR.md` for the wire contract and
the independent live smoke.

### Event cursors and gaps

Event pages are derived from the durable audit journal, which is a bounded ring. Sequences are
monotonic, pages are clamped to `MAX_EVENT_PAGE` (500), and `nextCursor` is present only while
more entries remain. A cursor pointing below the retained window is reported as `cursorExpired`
with an empty page: once the ring evicts entries, those sequences never return, and a gap is
never presented as a complete stream. `afterSeq == startSeq - 1` is exact continuity, not a gap.

Restart keeps events readable. Recovery marks the run `interrupted`, clears authority,
clears `last_outcome`, and increments `controlEpoch`; the durable journal remains
replayable from its retained start.

The stacked desktop successor calls that cursor contract only for positive
app-owned Run IDs. Its persisted key contains both owning session and exact Run
ID. `cursorExpired` moves the next request to `range.startSeq - 1`; the UI then
continues through the retained tail with a persistent **History gap** warning.
Storage failures cannot block emergency controls. Typed replay tests cover exact
binding, cross-owner denial, legacy projection, cursor expiry, reload persistence,
and Stop reachability while the warning is present.

### Visible activity states

`desktop/src/lib/computerActivity.ts` maps the projection to exactly one visible state. Control
disposition wins over lifecycle state, so a stopped or taken-over run can never present as an
ordinary pause even though all three share the `paused` lifecycle state. A terminal agent-owned
run is shown as ended rather than as live agent control, and a disposition this build does not
recognize fails closed as "unrecognized control state" instead of rendering as an ordinary run.

## Threat model

See [Computer Use Threat Model and Release Gate](COMPUTER_USE_THREAT_MODEL.md) for the current
threat-to-evidence matrix, trust boundaries, explicit unsupported dispositions, and remaining
packaged/hardware release blockers.

The design assumes model output, application content, screenshots, accessibility trees, MCP
clients, and persisted cache contents may be hostile. Important threats include prompt injection
from observed content, stale-observation clicks, target substitution, duplicate action delivery,
restart replay, hidden or secure fields, authorization widening, clickjacking, and cancellation
races.

The foundation prevents a backend call unless a valid local grant, exact target, fresh current
observation, allowed action class, and run budget all agree. It does not yet solve native consent,
screen redaction, prompt-injection interpretation, or platform-specific target attestation; those
remain release blockers in later issues.

### Model proposal boundary

The cockpit's model loop is deliberately narrower than the native action backend:

1. The local user starts a run, reviews its exact target, grants bounded authority, and observes it.
2. An unknown built-in model must pass an explicit two-frame simulator qualification for that
   session. Durable compatible-provider qualification is tied to the exact profile, endpoint, and
   model. Restarting, changing model, or changing route removes session-measured authority.
3. The model receives the local objective and a bounded semantic observation. It does not receive
   screenshot locators, host paths, grants, approval tokens, credentials, or native dispatch tools.
4. It must return exactly one typed proposal for the current observation. Malformed, parallel,
   stale, sensitive, disabled, unadvertised, raw-input, shell, clipboard, and coordinate proposals
   fail closed.
5. GrokPtah revalidates the session, route, run version, observation, and proposal after inference.
   An action becomes the same visible one-use local approval used by manual cockpit actions. It does
   not execute automatically.
6. A `complete` response may only mark the exact current run complete and revoke authority; it
   cannot contain or cause an OS mutation. **Stop** and **Take over** cancel Computer inference and
   invalidate late responses without cancelling an unrelated Build turn.

The opt-in `computer_agent_live` example exercises the real selected model against only the
deterministic simulator. Its report is redacted and explicitly records that no action executed.

| Adversarial condition | Fail-closed behavior |
|---|---|
| Prompt injection in observed labels or values | Content is marked untrusted; only the fixed proposal schema is accepted |
| Stale frame or invented element/action | Exact observation ID, enabled element, sensitivity, and advertised action are rechecked |
| Cross-session qualification reuse | Ephemeral authority is keyed to the exact session and model |
| Model or provider-route change | In-flight inference is cancelled and ephemeral authority is cleared |
| Duplicate/concurrent model requests | One Computer model operation may run per session |
| Stop, Take over, or run change during inference | The request is cancelled; any late proposal fails run/version/observation revalidation |
| Completion with hidden action arguments | Unknown or action-bearing completion fields are rejected |
| Valid action proposal | It is staged visibly and still requires the same local one-use approval as a manual action |

## macOS observation and semantic action slices (#269, #270)

The native adapter uses a runtime-loaded ScreenCaptureKit shim plus Accessibility semantic
snapshots behind the same platform-neutral backend. Its established path advertises
**foreground-semantic** capability (`pointer_fallback=false`, `key_chords=false`). The Stage 8
candidate adds a separate exact measured-background text-entry backend and proof; it never upgrades
the foreground backend by inference. Bringing the real target to the foreground remains an
authorized, disruptive `ActivateTarget`, not an isolated or background-safe action. The Computer
Run cockpit exposes non-prompting status, explicit per-permission requests,
bounded window discovery, exact scope review, one-use approvals, evidence and audit visibility,
pause, Stop, Take over, and non-cancelling steering. Native actions are limited to activation,
Accessibility invoke, visible value entry, selection, and semantic scrolling. Every mutation
requires a fresh observation and local one-use grant. It does not register a model action or
MCP tool. See [Computer Use on macOS](COMPUTER_USE_MACOS.md) for the privacy boundary, dispatch
attestation, packaging requirements, and disposable smoke fixture.

## Deliberate non-goals of the current desktop slice

- no Windows UI Automation or Linux portal adapter;
- no unattended or continuously autonomous model invocation;
- no MCP mutation/evidence surface;
- no raw arbitrary keyboard, pointer, coordinate fallback, clipboard, AppleScript, or shell endpoint;
- no unattended, cross-target, or inferred background grant;
- no cross-application target switching inside a run.

## Delivery sequence

| Stage | Issues | Outcome |
|---|---|---|
| Safety kernel | #268, #274 | Typed contract, simulator, durable authority, adversarial gates |
| Isolation contract | stage 1 | Host-enforced tier, sealed token, interned surface, exact freshness, typed proof. Native macOS is a singleton host-global-foreground conflict domain (capacity 1). Not isolated or preemptive. |
| macOS observe | #269 | Consented target selection, capture, redaction, semantic snapshots |
| Operator UX and model proof | #273, #272 | Visible runs/approvals and capability-based provider conformance |
| macOS act | #270 | Bounded semantic actions with durable bookkeeping-safe local takeover |
| Coordinator interoperability | #271 | Scoped Computer Run MCP tools and event visibility |
| WorkAttempt surface coordination | current stacked draft | Host-resolved Agent/Work/spec authority, deterministic per-domain queue, exact-current frame fence, durable dispatch identity, restart/expiry outcomes, secret-free local queue/owner projection |
| Out-of-band native cancellation channel | stacked candidate | Exact per-action atomic signal; Stop/Take over no longer wait on the action gate and preempt native preflight/activation waits. An atomic AX call already entered remains uncertain and non-replayable; external Rust/native qualification pending |
| Measured background text entry | stacked candidate | Reversible disposable-target calibration, one-use exact receipt, text-entry-only proof/grant, and native before/after foreground-app, active-window, pointer, and postcondition measurement. No activation or raw fallback; packaged native and strict external qualification pending; #287 remains open. |
| Isolated visual substrate decision | source candidate | Disposable measured VM; no default network, host clipboard/share, credential forwarding, or host input injection. Typed isolated input, a no-launch host probe, manifest/lifecycle contracts, authenticated read-only metadata protocol, and path-free open-handle content measurement are candidate source only. Helper/guest packaging, code-signing proof, carrier/rendering, launch/cleanup evidence, and dispatch are not implemented; #288 remains open. |
| Isolated helper / input domain | later | Host-native independently isolated visual input, not a simulator fixture |
| Semantic-first isolated visual fallback | later | Isolated visual input after semantic miss, never boolean-upgraded native AX |
| App-owned exact Run surface / always-available Stop | stacked candidate | Exact binding, per-Run approvals, persistent shell controls; external Rust qualification pending |
| App-owned agent attention marker | stacked candidate | Typed/redacted proposal, attention, approval, and rejection evidence; normalized marker never moves or impersonates the OS pointer; external Rust and visual qualification pending |
| Isolated visual agent cursor | later | Agent-owned cursor on a genuinely isolated input surface |
| Other platforms | #275, #276 | Windows and Linux adapters behind the same contract |

Provider support is capability-based, not model-name based. OpenAI-compatible corporate gateways
can be evaluated in tiers: coding tools, observation interpretation, semantic action selection,
and visual fallback. A cheaper model may perform coding or testing without being authorized for
computer actions; [#272](https://github.com/chriscase/GrokPtah/issues/272) owns that conformance
matrix and role routing.

The first opt-in provider probe uses only the deterministic simulator. Unknown and built-in catalog
models receive `none` until explicitly measured. A built-in model may earn process-local
`semantic_act` authority for the current session by selecting the exact safe element and recovering
from a stale-frame tool error against a replacement observation. Compatible-provider qualification
persists the same measured tier for its exact route. Image input and `visual_fallback_act` remain
unqualified. A qualified tier permits proposals only; it never replaces the cockpit's exact target
review, one-use local grant, reobservation, or native dispatch checks.

## Acceptance commands

Stage 1 is not isolated Computer Use, not physically preemptive, and not release-ready. Packaged
hardware focus, TCC, and takeover evidence remain explicitly unverified.

```sh
cargo fmt --check --manifest-path crates/codegen/grokptah-agent-bridge/Cargo.toml
cargo test --locked --manifest-path crates/codegen/grokptah-agent-bridge/Cargo.toml \
  --lib computer_use -- --test-threads=1
cargo test --locked --manifest-path crates/codegen/grokptah-agent-bridge/Cargo.toml \
  --lib computer_agent -- --test-threads=1
cargo test --locked --manifest-path crates/codegen/grokptah-agent-bridge/Cargo.toml \
  --lib mcp_control::tests::computer -- --test-threads=1
cargo test --locked --manifest-path crates/codegen/grokptah-agent-bridge/Cargo.toml \
  --test computer_use_release_gate -- --test-threads=1
cargo test --locked --manifest-path crates/codegen/grokptah-agent-bridge/Cargo.toml \
  --test mcp_streamable_transport live_computer -- --test-threads=1
cargo test --locked --manifest-path desktop/src-tauri/Cargo.toml \
  --lib computer_use -- --test-threads=1
```
