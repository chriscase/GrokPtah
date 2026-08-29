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
| Same-route capability change (tier, provenance, schema, credential, policy) | The capability generation moves and the binding stops validating — see [Provider capability generation](#provider-capability-generation-458) |
| Duplicate/concurrent model requests | One Computer model operation may run per session |
| Stop, Take over, or run change during inference | The request is cancelled; any late proposal fails run/version/observation revalidation |
| Completion with hidden action arguments | Unknown or action-bearing completion fields are rejected |
| Valid action proposal | It is staged visibly and still requires the same local one-use approval as a manual action |

### Provider capability generation (#458)

A route fingerprint — base URL, wire model, dialect — is stable across exactly the changes that
matter. A provider can keep serving the same endpoint and the same wire model while the capability
record behind it is rewritten: a measured tier replaced by a declared one, a schema bumped, a
credential rotated to a different principal, an operator policy narrowed, a requalification failing
outright. None of that moves the fingerprint, so a session qualified once used to keep
model-action authority it no longer had.

Session qualification is now bound to a **capability generation**: a secret-free binding of
everything that must still be true for a qualification to still mean what it meant when it was
taken. Two halves are checked, and they guard different failures. The generation stamp — an
authority id drawn at process start plus a monotonic counter — catches events that revoke
authority without changing any observable fact (explicit revocation, failed requalification,
process restart). The capability digest catches drift in the facts themselves.

The digest binds:

| Bound input | Why |
|---|---|
| Upstream authority lineage | An upstream rotation retires every capability beneath it |
| Normalized route identity (provider, base URL, wire model, dialect) | The original binding, preserved |
| Effective tier *after* operator policy | A downgrade must not be inheritable |
| Provenance, including which declared trust was honoured | Declared and measured are not interchangeable |
| Qualification schema id and version | A change to what qualification proves retires old proofs |
| Credential incarnation (secret-free principal fingerprint + monotonic incarnation) | Rotation, and deletion followed by re-adding identical material |
| Policy/allowlist revision | An operator narrowing takes effect immediately |
| Assurance profile | A qualification is valid only at the profile it was taken under |
| Measured or signed qualification evidence digest | The proof itself, without the transcript |

The binding is re-validated at every boundary: qualification, observation, model proposal,
staging, approval, lease acquisition, live-frame delivery, and dispatch. Dispatch and live-frame
delivery re-derive the capability facts from live state **at the instant of the call**, so a
downgrade that lands mid-operation is refused before the action becomes physical and before the
next frame of screen content leaves the host, rather than at the next operation.

A binding reference is secret-free and confers nothing on its own: the authority half lives only
in memory. A reference persisted in a run record and restored after a restart therefore names
nothing, is refused, and is quarantined as needing explicit re-establishment. Nothing promotes a
quarantined qualification in place.

#### The authority is not reachable from outside

Everything that mints or moves authority is crate-internal. There is no way for a caller of the
library to construct a registry, reach the host's, mint a binding, manufacture evidence, change
the policy, or install its own boundary — a public gate trait, in particular, would let a caller
install an allow-all boundary in production. `AgentHostHandle::computer_use_service` is the single
seam: it is the only way to obtain a kernel wired to the authority, and the authority itself is
never handed out. A kernel built any other way admits operator-driven runs and refuses every
model-attributed one.

#### Who is driving is named, not inferred

The kernel does not read "no binding, therefore the operator". Inferring an actor from an absence
means any path that drops a binding silently widens the run into the operator's authority. The
actor is explicit, and there are three:

| Actor | Proof | Effect |
|---|---|---|
| Operator | The run's live one-use `ActionGrant` | Needs no provider capability |
| Model | A capability binding the live authority still honours | Re-validated at every boundary |
| Stripped | None — the binding is gone while the grant it was driving under is still live | Dispatches nothing |

Clearing model authority always revokes the grant underneath it, so handing control back (Pause,
Take over) issues a *fresh* grant and the run is honestly operator-driven again. There is no
method that clears a binding on its own: that would be a way to walk a model-driven run back into
the operator's authority.

#### A dispatch authorization names its effect

Dispatch authorization is not "this model may act" — it is "this model may perform *this*". The
authorization is issued against the exact run, observation, and action class, and is redeemed once,
by value, immediately before the backend call. It cannot cover a second dispatch and cannot be
paired with a different action than the one it was taken for. Redemption re-derives the capability,
so an authorization is good only while the capability it was issued against is still exactly the
same one.

#### A stored capability record is not evidence

A record in `gateway.json` asserting `measured` is a file the configuration can rewrite. It no
longer short-circuits into durable action authority: this authority calls something measured only
when it measured it. An unqualified model reports observation authority whatever the stored record
claims, and action authority requires the bounded local qualification to run in this session (or
signed evidence). What durable capability still does is decide whether the model is *eligible* to
be qualified at all.

Every refusal is one value with one message. A foreign binding, an unknown binding, a revoked
binding and a stale binding are indistinguishable, so a denial cannot be used to probe which
qualifications exist on the host.

Generation advance is checked before any mutation. On exhaustion nothing is mutated, the counter
neither saturates nor wraps, and the authority refuses every boundary from then on — a host that
cannot prove a revocation does not grant.

#### Declared capability is not evidence

A `Measured` capability statement is the record of a probe that actually ran. A `Declared` one is a
statement someone wrote down. The deployment must say which it means:

| `GROKPTAH_COMPUTER_DECLARED_TRUST_PROVENANCE` | Behaviour |
|---|---|
| unset (default) | Declared capability qualifies **observation only**. Whatever tier the record claims, it can never become durable action authority. |
| set to a provenance name | Declared capability may carry action authority. The name is published in the binding and bound into the digest, so withdrawing or changing the trust invalidates every binding taken under it. |

The `high_assurance` profile does not honour declared trust at all, even when configured.

`GROKPTAH_COMPUTER_ASSURANCE_PROFILE` selects `economy`, `balanced` (default), or
`high_assurance`. The three profiles check the **same** boundaries, produce the same
indistinguishable refusal, and bind the same digest; they differ only in how long a qualification
may stand, how many dispatches it may authorize, and what evidence class the deployment insists on:

| Profile | Qualification lifetime | Dispatches per qualification | Minimum action evidence | Honours declared trust |
|---|---|---|---|---|
| `economy` | 5 min | 8 | measured | yes |
| `balanced` | 15 min | 32 | measured | yes |
| `high_assurance` | 2 min | 1 | signed | no |

An unrecognised profile name narrows to `high_assurance` rather than leaving the deployment on
something broader than the operator meant. Changing the profile or the trust policy advances the
generation, so bindings taken under the old setting stop validating immediately.

#### What this does not claim

The lineage field binds the *upstream authority a capability descends from*, and today the host
populates it with its own process auth lineage: an id drawn at startup and a counter that advances
on every credential or policy invalidation. That is a real upstream — a restart or an invalidation
does retire every binding — but it is deliberately not a claim of verified principal, tenant,
scope, or operator identity. None of those exist to bind yet; minting them is separate work
(#477), as is the service-scoped auth epoch (#460). The field is the seam they plug into, and
their rotation will retire capability generations without further design.

| Adversarial condition | Fail-closed behavior |
|---|---|
| Same-route tier downgrade between observation and dispatch | Refused at dispatch; the backend is never called |
| Measured capability rewritten to declared | Tier resolves to observation; action boundaries refuse |
| Credential rotated to a different principal mid-run | Every standing binding is retired at the rotation |
| Credential deleted and re-added with identical material | The incarnation advances; the old authority is not restored |
| Requalification failure | The standing binding is retired before the next boundary |
| Schema bump or operator policy change | Digest and generation both move; bindings refuse |
| Restart with a persisted binding reference | Names nothing; quarantined for explicit re-establishment |
| Generation exhaustion | Nothing mutates, nothing wraps, every boundary refuses |
| Model authority stripped while its grant is still live | The run is nobody, not the operator; it dispatches nothing |
| A dispatch authorization reused, or paired with another action | The lease redeems once, and only against the effect it names |
| Upstream authority rotation | Lineage moves; every capability beneath it is retired |
| Caller-supplied gate or caller-minted binding | Not expressible: the authority, its gate, and its evidence are crate-internal |

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
- no cross-application target switching inside a run.

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
