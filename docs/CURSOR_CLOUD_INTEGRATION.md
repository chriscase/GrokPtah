# Cursor Cloud agents from GrokPtah

**Status:** provider-neutral external-worker DTOs, a trusted native Cursor Cloud
v1 lifecycle adapter, a host-level durable idempotency ledger, and browser
parsers are staged in `grokptah-agent-sdk`, `grokptah-agent-bridge`, and the
Tauri-free public package. This is not live-qualified and is not a 100% claim.

GrokPtah can manage Cursor's Cloud Agents as an external execution provider.
This is separate from controlling a local Cursor desktop window and separate
from native Computer Use. Cursor's official Cloud Agents API is currently
documented as beta and exposes durable agents and runs over
`https://api.cursor.com`. It is the right integration surface for unattended
cloud work; the desktop UI remains a human takeover surface.

## Supported vs unsupported in this slice

Supported by the staged adapter/host/browser parsers (fake-API covered, not
live-qualified):

- isolated launch with `repos` (hosted cloud) and no named `env`;
- exact repository/ref binding, host + adapter allowlists;
- follow-up on an eligible worker with no active run;
- cancel that ends only in an observed terminal `Cancelled` run;
- run-attributed artifacts with a non-empty digest (provider digest or a
  trusted download-and-hash);
- durable launch/follow-up/cancel idempotency keyed by `request_id` plus a
  canonical payload hash, with explicit Pending/Uncertain fail-closed states.

Explicitly unsupported in v1:

- sequenced provider event streams (`stream: "unsupported"`, `lastSeq: null`);
  synthesizing `lastSeq = 0` is a contract violation;
- named Cursor `env` alongside `repos` (mutually exclusive on the live API);
- live artifact listings that lack `runId` (the live list is agent-scoped
  `path`/`sizeBytes`/`updatedAt` and currently fails closed);
- exposing raw download URLs or credentials to the browser;
- Computer Use authority, core-agent harness turns, merge/undraft, or
  list/archive lifecycle;
- 100% coverage of the Cursor Cloud API.

## What the Cursor API provides

The official API supports:

- creating an agent and its initial run;
- listing and reading agents and runs;
- sending follow-up runs to an existing agent;
- streaming a run, polling terminal state, and cancelling an active run;
- listing/downloading relative artifacts;
- usage reporting; and
- reversible archive/unarchive lifecycle operations.

Cursor Cloud agents clone GitHub repositories, work on isolated cloud
machines, and can push branches and optionally create pull requests. The API
also reports a durable agent URL, branch information, status, timestamps, and
terminal results. These claims must be rechecked against the live API during
qualification because the API is beta.

Official v1 semantics this adapter relies on:

- `repos` selects the hosted cloud environment; a named `env` is mutually
  exclusive with `repos` and is not sent;
- conflict bodies may carry `code` or `error.code` values
  `agent_conflict` / `agent_id_conflict`, `agent_busy`, and
  `run_not_cancellable`;
- artifact listing may omit digest and run attribution; download is
  `GET /v1/agents/{id}/artifacts/download?path=` returning `{url, expiresAt}`
  for a short-lived HTTPS object;
- a provider stream exists, including `410 stream_expired`, but it is not
  qualified here.

## GrokPtah architecture

```text
GrokPtah Manager (not wired in this slice)
  └─ ExternalWorkerHost (allowlist + durable ledger)
       └─ CursorCloudAdapter (server-side API key only)
            ├─ ExternalWorkerRecord: provider + Cursor agent id + repo/ref
            ├─ ExternalWorkerRunRecord: run id + lifecycle + stream=unsupported
            ├─ redacted status → desktop and browser broker
            └─ run-attributed digested artifacts (never raw URLs)
```

`ExternalWorkerHost` is the orchestration boundary. It is not `AgentHost`, not
Computer Use, and not the core MCP turn loop. Registration of an adapter does
not grant launch rights; the host require an explicit repository allowlist.
The adapter keeps a second allowlist so a miswired caller still cannot launch
into an arbitrary repository the API key can access.

The reusable DTO boundary is `grokptah-agent-sdk::external_worker`. It exposes
validated launch/follow-up requests plus redacted worker, run, event, and
artifact projections. Artifacts serialize `runId` (Rust field
`external_run_id`). The crate has no provider credentials or network client, so
another product can import the same contracts from a desktop adapter, service,
or browser-safe broker without importing GrokPtah's authority implementation.

The native bridge exposes `CursorCloudAdapter` behind `ExternalWorkerAdapter`.
It targets Cursor Cloud Agents API v1, keeps the API key in the trusted
process, sends only an isolated exact-repository/ref request with
`autoCreatePR: false`, verifies returned identity and write/PR safety flags,
reconciles `409` conflicts with GET state instead of duplicating remote work,
and refuses to publish artifacts that are not run-attributed with a digest.
Presigned download hosts are constrained to the documented
`cloud-agent-artifacts.s3.*` family, HTTPS, and the existing SSRF preflight.
The in-tree fake API fixture covers the bounded checks without credentials.
This is an implementation seam and contract fixture, not evidence that a live
Cursor account has been exercised.

GrokPtah's ledger owns retries. Cursor's client-supplied `agentId` is sent
only when `request_id` already has the strict `bc-<uuid>` shape.

## Safety contract

1. Keep the Cursor API key in the native/server credential boundary. It must
   never reach the browser, Tauri webview, prompt transcript, or public broker.
2. Require an explicit host repository allowlist, exact starting ref, isolated
   execution mode, and an idempotency key before creation.
3. Default to isolated cloud work and `autoCreatePR: false`; creating a PR,
   promoting changes, or merging remains a separate human-approved action.
4. Store provider, agent ID, run ID, repo/ref, status, and timestamps.
   Redact raw tool output and provider credentials from browser projections.
5. Do not claim provider stream continuity. Poll GET state. A closed or
   expired stream is never evidence of completion.
6. Treat Cursor cancellation as terminal only after GET shows `Cancelled`.
   `409 run_not_cancellable` is reconciled with GET; other terminals are not
   cancellable; a still-running run after 409 is Uncertain.
7. Allow artifact materialization only for `artifacts/` paths attributed to
   the requested run, with a non-empty digest, and only inside the trusted
   adapter. Never copy the presigned URL into a browser DTO.
8. Apply the same GrokPtah approval, discard, and promotion rules used for
   local runs. A Cursor agent must not acquire native Computer Use authority
   through this adapter.
9. Identical retries replay the original ledger result. Payload drift is
   rejected. Pending and Uncertain stay fail-closed until reconciled.

## Qualification stages

1. **Contract fixture (this slice):** the native fake Cursor API fixture
   proves isolated create, host/adapter allowlists, response-safety checks,
   exact source projection, status polling, ineligible/busy follow-up
   rejection, 409 GET reconciliation, redacted terminal text, observed
   terminal cancellation, durable no-duplicate retry, payload-drift rejection,
   concurrent Pending, and run-attributed digest-bearing artifacts including
   bounded download-and-hash. Provider list/archive, stream reconnect/expiry,
   and a live Cursor campaign remain open.
2. **Read-only live probe:** list models/repositories and read an existing
   disposable agent; record API version, limits, retention, and redaction.
3. **Disposable create:** create one agent from an exact public test ref with
   no PR creation, capture branch/status/artifact/usage evidence, then archive
   it and verify no source checkout was changed.
4. **Manager integration:** expose Cursor as a provider-neutral lane in the
   desktop and browser-safe broker. Typed launch, follow-up, status, artifact,
   and cancellation calls exist on the browser client; server broker routes
   and live qualification remain to be completed. `ExternalWorkerHost` is not
   wired into `AgentHost` in this slice.
5. **Release gate:** independently review the adapter, run a retry/restart
   soak on a non-protected target, and prove that approval, promotion, and
   Computer Use remain separate. Do not infer 100% from a green fixture.

## Boundaries

This adapter does **not** claim to manage foreground Cursor desktop sessions,
Cursor's private UI state, or arbitrary local workspaces. Those remain manual
or require a separately qualified local-worker protocol. It also does not
make Cursor a GrokPtah runtime provider: Cursor is an external coding-agent
worker that GrokPtah can schedule, observe, review, and—after approval—hand
off.

Authoritative references:

- [Cursor Cloud Agents API](https://cursor.com/docs/cloud-agent/api/endpoints)
- [Cursor Background Agents overview](https://docs.cursor.com/background-agent)
- [Cursor CLI](https://cursor.com/docs/cli/overview)
