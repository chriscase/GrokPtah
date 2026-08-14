# Computer Use

Computer Use is tracked by epic [#267](https://github.com/chriscase/GrokPtah/issues/267).
It is intentionally staged. The safety kernel, simulator, desktop operator cockpit, native macOS
observation, and the first semantic macOS action slice share one bounded run contract. Computer
Use is still locally operated and is not yet exposed as a model tool or MCP mutation surface.

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

## Threat model

The design assumes model output, application content, screenshots, accessibility trees, MCP
clients, and persisted cache contents may be hostile. Important threats include prompt injection
from observed content, stale-observation clicks, target substitution, duplicate action delivery,
restart replay, hidden or secure fields, authorization widening, clickjacking, and cancellation
races.

The foundation prevents a backend call unless a valid local grant, exact target, fresh current
observation, allowed action class, and run budget all agree. It does not yet solve native consent,
screen redaction, prompt-injection interpretation, or platform-specific target attestation; those
remain release blockers in later issues.

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
- no Build-agent tool or automatic model invocation;
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
