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
5. Policy is checked again immediately before the backend action.
6. Successful actions invalidate the observation, forcing the caller to observe again.

Authorization is fail-closed. Grants do not survive restart, pause, cancellation, completion,
failure, target changes, or exhausted limits. Secure and system-restricted surfaces are denied
even when a grant exists. Pointer fallback and key chords require separate action classes; a
semantic-action grant cannot silently expand into raw input control.

## Model-output boundary and completion proof (#456, #457)

Untrusted model output has exactly one way into a run, and success has exactly one way out.

### Host-owned sealing

`propose_computer_action` returns `RawModelProposal`: the provider's bytes, carrying no authority,
plus the `RouteBinding` describing the route they were requested over. `accept_model_output` is the
only public entry to the boundary. It takes **identifiers only** and looks every authority-relevant
value up itself — the run from the durable ledger, the capabilities from the backend. There is no
seam anywhere that accepts a caller-supplied `ComputerRun`, capability set, or pre-minted
capability, because a caller that could hand in a fabricated context could mint a seal that says
whatever it likes. The strict normalizer behind it is private, and `ModelProposalContext` has no
public constructor; a `compile_fail` doctest on `accept_model_output` holds that shut.

The resulting `AcceptedModelProposal` has private fields, no `Deserialize`, no public constructor,
and is not `Clone`. The authority-free `ComputerAgentProposal` is `Serialize`-only and reaches no
application seam, so a deserialized value cannot stage or complete.

### What the seal binds

One digest covers everything, recomputed from the live run when the capability is spent: run ID,
owner session, run version, control epoch, run state and disposition, exact grant ID and generation
(issue time, expiry, remaining uses, revocation, action classes), target identity and generation,
exact frame ID, sequence and capture time, effective policy limits, the operator objective and its
predicate digest, the backend capability surface, and the provider route. One digest rather than a
list of comparisons is deliberate: adding an authority means adding it there, and every existing
seal is invalidated by construction — there is no way to add a binding and forget to check it.

Four `RouteBinding` slots are typed and digested but unbound on this branch, because the
authorities that fill them do not exist yet: provider capability generation
([#458](https://github.com/chriscase/GrokPtah/issues/458)), adaptive profile
([#435](https://github.com/chriscase/GrokPtah/issues/435)), host-issued principal/auth generation
([#477](https://github.com/chriscase/GrokPtah/issues/477)), and lease/agent binding. An unbound
slot digests as a distinct marker, never as an empty string, so it cannot collide with a real
value. Until #458 lands, `route_fingerprint` and `model` are caller-attested and the seal proves
only that they did not change between minting and application.

The seal is not a secret: no key, no MAC. Unforgeability is Rust module privacy; freshness is the
re-check. Seals are versioned and time-bounded, and minting plus application both run under one
operation lock.

The normalizer parses model arguments under a reader that rejects duplicate JSON keys (which
`serde_json` would otherwise resolve last-key-wins, letting one payload mean two things), unknown
keys, trailing content, prose, and oversized payloads. Pointer, key-chord, and wait actions stay
operator-only regardless of grant or backend capability. Summaries are capped, refused if they
carry control characters, and scrubbed with the same public privacy needles the durable journal
uses.

### Completion proves the operator's objective

A verified receipt proves that *one approved action ran and the host captured the next frame*. That
is not the same claim as "the thing the operator asked for is done", so it is not enough to
terminate a run.

A `ComputerTaskSpec` closes the gap. The operator authors the objective and, with it, a **closed**
predicate over observable frame state — a fixed enum, no expression language, nothing a model can
contribute to. The objective text is bound by digest so a spec cannot be paired with a different
ask than the model was given, and it is settable only before authorization, so "done" is fixed
before any authority exists. A run with no authored objective can never be completed on a model's
say-so, because nothing defines success for it.

Predicate locators address elements by **role and label**, never by element ID. Semantic element
IDs are ephemeral per observation, so an ID-addressed predicate would silently stop matching after
any re-observation. A locator that resolves to nothing — or ambiguously, to more than one element —
is an explicit failure. Missing evidence is never success.

Completion therefore requires both, in order:

1. **A credible claim.** A host-issued receipt, positive, verifying the exact current frame. Without
   one the claim is refused outright and nothing changes — a model cannot halt a run by asserting
   success repeatedly.
2. **A satisfied objective.** The operator's predicate must hold on that same frame. A credible
   claim with an unmet objective stops the run for review (`Paused` with an `awaiting_review`
   disposition) rather than completing. That is explicitly not a success.

`complete_verified` is the only route to `Completed`; there is no unguarded completion entry point,
and operators end runs through cancellation, which claims nothing about success.

### Receipt lifecycle

A dispatch mints an `ActionReceipt` bound to the frame it was authorized against, the accepted
action's fingerprint, a host-minted receipt identity, the authority epoch, and the backend outcome.
It starts unverified and acquires evidence only through `observe_postcondition` — the single frame
the host captures immediately after a dispatch, in the same epoch — and only when the outcome was
positive and the action's expectation holds on that frame. An expectation whose effect a semantic
frame cannot show is `Opaque` and can never be met: "we could not check" must not stand in for "it
worked".

Any ordinary observation, pause, takeover, cancellation, limit, in-flight failure, new grant, or
restart recovery clears the receipt. So the #456 dispatch → re-observe → complete sequence returns a
typed `UnverifiedCompletion` and mutates nothing.

### Failure classification and durability

Once `ComputerBackend::act` has been entered, the host cannot know whether the machine was touched,
so **every** failure from that point is reclassified `UncertainOutcome` and needs operator
reconciliation. An in-flight failure also clears `last_outcome`: a positive outcome on a failed run
is a statement about the machine that nothing currently backs.

Duplicate-proposal admission is durable on the run record and consumed **only after** a proposal
actually stages, so a refused or failed application never burns a fingerprint and blocks a
legitimate retry.

Refusals are journaled onto the run's existing durable event stream via `record_proposal_refusal`,
carrying the typed `ComputerErrorCode` and the operation only — no model text, no observed content.
Each refusal advances the run revision, so the refused capability and any sibling minted from the
same snapshot die immediately rather than being retryable against an unchanged record. That reuses
the existing projection seam rather than adding a second ledger; richer audit integration is
[#462](https://github.com/chriscase/GrokPtah/issues/462).

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

Audit entries retain operation metadata, dispositions, action classes, observation IDs, and
error codes. They do not retain action payloads, typed text, screenshots, application values,
window titles, credentials, or arbitrary model content. Evidence references are opaque IDs and
hashes rather than filesystem paths.

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
snapshots behind the same platform-neutral backend. The Computer Run cockpit exposes
non-prompting status, explicit per-permission requests, bounded window discovery, exact scope
review, one-use approvals, evidence and audit visibility, pause, Stop, Take over, and
non-cancelling steering. Native actions are limited to activation, Accessibility invoke, visible
value entry, selection, and semantic scrolling. Every mutation requires a fresh observation and
local one-use grant. It does not register a model action or MCP tool. See
[Computer Use on macOS](COMPUTER_USE_MACOS.md) for the privacy boundary, dispatch attestation,
packaging requirements, and disposable smoke fixture.

## Deliberate non-goals of the current desktop slice

- no Windows UI Automation or Linux portal adapter;
- no unattended or continuously autonomous model invocation;
- no MCP Computer Run surface;
- no raw arbitrary keyboard, pointer, coordinate fallback, clipboard, AppleScript, or shell endpoint;
- no background or unattended grant;
- no cross-application target switching inside a run;
- no semantic re-verification of a postcondition beyond the single host-issued verifying frame;
- no model-driven completion of a run whose final action is not frame-checkable: an `Opaque`
  expectation carries no receipt, so such a run must be ended by operator review. This is
  fail-closed and deliberate;
- no binding to provider capability generation (#458), adaptive profile (#435), host principal/auth
  generation (#477), or lease/agent identity — those slots are typed and digested but unbound until
  those authorities exist.

## Delivery sequence

| Stage | Issues | Outcome |
|---|---|---|
| Safety kernel | #268, #274 | Typed contract, simulator, durable authority, adversarial gates |
| macOS observe | #269 | Consented target selection, capture, redaction, semantic snapshots |
| Operator UX and model proof | #273, #272 | Visible runs/approvals and capability-based provider conformance |
| macOS act | #270 | Bounded semantic actions with immediate local takeover |
| Coordinator interoperability | #271 | Scoped Computer Run MCP tools and event visibility |
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
