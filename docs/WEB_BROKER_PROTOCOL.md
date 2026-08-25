# GrokPtah Web Broker Protocol (v1)

**Status:** implementation contract for ContextDesk War Room integration

This document defines the safe way for a web product to expose GrokPtah
capabilities. A browser never connects to GrokPtah's loopback MCP endpoint and
never receives a GrokPtah bearer token. The ContextDesk server (or another
trusted broker) is the only web-side caller.

## Trust boundary

```text
War Room browser --(user session, opaque broker ids)--> ContextDesk broker
ContextDesk broker --(server credential, exact scope)--> GrokPtah MCP
GrokPtah desktop authority --(leases/approvals)--> execution + Computer Use
```

The broker must keep the GrokPtah token, workspace paths, provider credentials,
raw prompts, and native Computer Use details server-side. Browser payloads are
redacted projections and opaque broker identifiers. A browser disconnect is a
reconnect event, never permission to restart or promote a run.

Mutating browser routes must also use the broker's normal CSRF defense (for
example, a SameSite user session plus an `X-CSRF-Token` issued by ContextDesk).
That token is a broker credential, not a GrokPtah bearer token, and must never
be accepted by the GrokPtah MCP listener.

## Binding an investigation

Before a web request can submit or observe a run, the broker creates a binding
for an authenticated user/team and an explicitly approved workspace:

```http
POST /api/grokptah/v1/bindings
Cookie: contextdesk_session=<ContextDesk user session>
X-CSRF-Token: csrf-01J...
Idempotency-Key: bind-01J...
Content-Type: application/json

{
  "investigationId": "war-room-42",
  "workspace": "approved-workspace-alias",
  "requestedCapabilities": ["session.observe", "run.execute", "run.review"]
}
```

The response contains an opaque `bindingId`, a negotiated capability set, and
an expiry. The server verifies that the requested capabilities are advertised
by GrokPtah and allowed by the team policy. `run.promote` and
`computer.control` are never silently added to a binding.

```json
{
  "bindingId": "gb_01J...",
  "contract": "grokptah.capabilities.v1",
  "expiresAt": "2026-08-24T23:00:00Z",
  "capabilities": [
    { "id": "session.observe", "availability": "available" },
    { "id": "run.execute", "availability": "available" },
    { "id": "run.review", "availability": "available" }
  ]
}
```

## Run operations

All routes below require a live binding. The broker translates the opaque
binding and run ids to the exact GrokPtah `session_id`, `workspace`, and
`run_id` tuple. It must not accept those internal values from the browser.

| Broker route | GrokPtah operation | Default | Notes |
| --- | --- | --- | --- |
| `GET /bindings/{bindingId}/sessions` | `ptah_list_sessions` | read-only | Filter to the bound workspace. |
| `GET /bindings/{bindingId}/capacity` | `ptah_get_capacity` | read-only | Do not leak host paths or credentials. |
| `POST /bindings/{bindingId}/runs` | `ptah_submit_task` | execute | Requires an idempotency key and explicit prompt/bounds. |
| `GET /bindings/{bindingId}/runs/{runId}` | `ptah_get_run` | read-only | Returns a redacted run projection. |
| `GET /bindings/{bindingId}/runs/{runId}/progress` | `ptah_get_progress` | read-only | Bounded progress only. |
| `GET /bindings/{bindingId}/runs/{runId}/changes` | `ptah_get_changes` | read-only | Reviewable change summary. |
| `GET /bindings/{bindingId}/runs/{runId}/tests` | `ptah_get_test_results` | read-only | No arbitrary command output. |
| `GET /bindings/{bindingId}/runs/{runId}/handoff` | `ptah_get_handoff` | read-only | Bounded final handoff. |
| `GET /bindings/{bindingId}/runs/{runId}/review` | `ptah_review_run` | read-only | Isolated-run diff/fingerprint. |
| `POST /bindings/{bindingId}/runs/{runId}/approve` | broker approval | execute | Binds exact review fingerprints to a short-lived approval. |
| `POST /bindings/{bindingId}/runs/{runId}/promote` | promotion authority | promote | Requires the short-lived approval and desktop human gate. |
| `POST /bindings/{bindingId}/runs/{runId}/cancel` | `ptah_cancel` | execute | Explicit user action; idempotent request id. |
| `GET /bindings/{bindingId}/queue` | `ptah_get_queue` | read-only | Includes queue revision. |
| `POST /bindings/{bindingId}/queue` | `ptah_queue_prompt` | execute | Server adds request id and policy bounds. |
| `POST /bindings/{bindingId}/steer` | `ptah_steer` | execute | Non-cancelling steer only. |

The run creation body is intentionally narrower than MCP:

```json
{
  "prompt": "Review the staged change for correctness and security.",
  "executionMode": "isolated_worktree",
  "bounds": { "maxRounds": 12, "maxDurationMs": 1800000 },
  "allowQueue": true
}
```

The broker supplies a fresh, unique `request_id`, records the user and
investigation audit context, and forwards only an allowlisted execution mode
and bounds. It must reject empty prompts, out-of-policy workspaces, and bounds
above the team maximum before calling GrokPtah.

## Event streaming and recovery

The browser receives redacted broker events, not the raw MCP stream:

```http
GET /api/grokptah/v1/bindings/gb_01J.../runs/br_01J.../events?afterSeq=42
Accept: text/event-stream
```

Each frame contains the opaque broker run id, the retained sequence number, a
timestamp, and a safe update projection. The broker persists the last delivered
cursor per subscriber and maps `cursor_expired`/recovery to a bounded `409`
response containing `afterSeq` and a poll route. Clients must poll the run or
event page, then reconnect; they must never infer a completed run from a closed
socket.

The broker event frame is intentionally distinct from the raw MCP notification:

```text
id: 43
data: {"kind":"event","brokerRunId":"br_01J...","seq":43,
       "ts":"2026-08-24T20:00:00Z","update":{"type":"progress"}}
```

`id` must equal `seq`. A recovery frame has `kind: "recovery"`, the same opaque
`brokerRunId`, an `afterSeq`, `reason`, and a relative `pollRoute`. Unknown
frame kinds and scope mismatches fail closed.

## Approval, promotion, and Computer Use

These are separate server endpoints and separate audit records:

```text
review -> user sees exact diff/fingerprint -> approve (short TTL)
      -> promote with approval id       -> receipt

computer observe -> redacted projection only
computer control -> explicit user grant + lease + revision + expiry
```

`POST /runs/{id}/approve` must display the exact source/final fingerprints and
changed-file list returned by `ptah_review_run`. `POST /runs/{id}/promote`
accepts only the short-lived approval receipt. A browser session, a focused tab,
or a team role is not a Computer Use grant. Computer control requests must
carry the expected run revision and action class; stale revisions are rejected
without mutation.

The approval payload uses the same changed-file object shape as the
`ReviewReceipt` projection; a path-only list is not sufficient evidence:

```http
POST /api/grokptah/v1/bindings/{bindingId}/runs/{runId}/approve
Cookie: contextdesk_session=<ContextDesk user session>
X-CSRF-Token: csrf-01J...
Idempotency-Key: approve-01J...
Content-Type: application/json
```

```json
{
  "sourceFingerprint": "src-01J...",
  "finalFingerprint": "final-01J...",
  "changedFiles": [
    { "path": "desktop/src/lib/grokptahBrokerClient.ts", "summary": "Typed broker approval contract" }
  ],
  "ttlMs": 300000
}
```

The browser-facing client may carry these two routes as typed calls, but the
client is not the authority: the broker must recompute or verify the review
receipt, bind the approval to the same opaque run and fingerprints, enforce a
short expiry, and require the desktop-side human gate before forwarding a
promotion. A stale, expired, mismatched, or replayed approval is rejected
without mutation.

The web broker should initially expose only `computer.observe`. Add
`computer.control` after packaged helper/VM evidence, lease soak, and a human
acceptance run are green.

## Error mapping

The broker keeps a stable public taxonomy while retaining the GrokPtah error in
server-side audit logs:

| HTTP | Broker code | Meaning |
| --- | --- | --- |
| `400` | `invalid_request` | Malformed prompt, bounds, or cursor. |
| `401` | `unauthenticated` | User session missing/expired. |
| `403` | `forbidden_scope` | User, binding, workspace, or capability mismatch. |
| `404` | `not_found` | Opaque binding/run is unknown to this user. |
| `409` | `stale_or_recovery` | Cursor/version/approval is stale; no mutation implied. |
| `429` | `capacity` | Bounded admission is full and queueing was not allowed. |
| `502` | `authority_unavailable` | GrokPtah authority is asleep, locked, or unavailable. |
| `500` | `internal` | Unexpected server failure; never include privileged diagnostics. |

Every mutating response includes the broker request id, GrokPtah request id,
and an audit outcome. Retries reuse the same idempotency key; a new intent must
use a new key.

## Initial ContextDesk deliverables

1. Implement the broker routes above using ContextDesk's existing
   `cd-triage-sdk`/`cd-triage-runtime` host-neutral boundary.
2. Add a disposable local GrokPtah adapter test that exercises capability
   negotiation, submit, reconnect-from-cursor, review, and discard.
3. Keep the War Room UI observe/review-only until the desktop authority and
   packaged Computer Use gates are qualified.
4. Add an audit view showing binding, user, request id, capability, scope, and
   outcome for every mutation.

The GrokPtah Tauri client staging surface is
`desktop/src/lib/grokptahOperations.ts`. It is transport-neutral and can be
used by a trusted ContextDesk desktop adapter; it is not a browser client and
does not replace the server-side scope and approval checks.

For the browser/War Room path, use the separate
`desktop/src/lib/grokptahBrokerClient.ts` shape (or an equivalent ContextDesk
implementation). It uses cookie/session credentials and has no bearer-token
option by design.
