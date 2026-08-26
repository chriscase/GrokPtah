# Cursor Cloud agents from GrokPtah

**Status:** provider-neutral external-worker contract, a trusted native
Cursor Cloud v1 lifecycle adapter, and a host-minted production-authority gate
in front of every mutation are staged in `grokptah-agent-sdk` and
`grokptah-agent-bridge`. All evidence to date is synthetic and in-tree; the
adapter is **not** live-qualified and no live provider account, credential, or
network has been exercised.

GrokPtah can manage Cursor's Cloud Agents as an external execution provider.
This is separate from controlling a local Cursor desktop window. Cursor's
official Cloud Agents API is currently documented as beta and exposes durable
agents and runs over `https://api.cursor.com`; it is the right integration
surface for unattended cloud work, while the desktop UI remains a human
takeover surface.

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

## GrokPtah architecture

```text
GrokPtah Manager
  └─ CursorCloudAdapter (server-side API key only)
       ├─ ExternalAgentRecord: provider + Cursor agent id + repo/ref
       ├─ ExternalRunRecord: Cursor run id + lifecycle + cursor/last poll
       ├─ redacted status/events → desktop and browser broker
       └─ review receipt → existing approval/promotion gate
```

The existing durable orchestration identity and `ExternalRunContext` are the
natural attachment point. The adapter should translate Cursor statuses into
GrokPtah's provider-neutral run state, retain the exact repository and starting
ref, and persist the Cursor agent/run IDs as opaque external IDs. A Cursor
follow-up is a new run on the same agent; it is not an implicit resume of a
cancelled run.

The reusable DTO boundary is `grokptah-agent-sdk::external_worker`. It exposes
validated launch requests plus redacted worker, run, event, and artifact
projections. It deliberately has no provider credentials or network client, so
another product can import the same contracts from a desktop adapter, service,
or browser-safe broker without importing GrokPtah's authority implementation.

## Production authority gate

An adapter is a transport, not an authority. `ExternalWorkerAuthority`
(`grokptah-agent-bridge::external_worker_authority`) is the gate in front of
launch, follow-up, and cancel, and `ExternalWorkerStore`
(`external_worker_store`) is its durable ledger. In order, a mutation needs:

1. **Advertised capability.** A qualified adapter is registered, answered a
   bounded reachability probe, speaks `grokptah.external-workers.v1`, and host
   policy allows the principal, workspace, and provider identity. Registration
   is single-shot: installing a second adapter for the same
   `(provider, providerId)` fails closed with `ProviderAlreadyRegistered`
   rather than replacing the first, and each successful install bumps a
   capability revision.
2. **A host-minted admission.** `ExternalWorkerAdmission` binds principal,
   session, workspace, run, one mutation kind, provider identity, capability
   revision, payload digest, provider target, idempotency key, a single-use
   nonce, and a TTL clamped to 15 minutes. The public projection carries no
   prompt, credential, provider URL, or host path. It is not a bearer
   credential: the host revalidates every field against its own durable mint
   ledger, so a perfectly well-formed ticket this host did not mint is
   rejected on lookup. Expiry, single use, and capability revision are checked
   at send time, not at mint time.
3. **A clean durable ledger.** A receipt or an acceptance tombstone for the
   same idempotency key refuses the send.

After the send there are exactly three dispositions. Accepted writes a
permanent `MutationTombstone`; rejected records no provider effect; anything
else — a transport failure, a 5xx/408/409/425/429, or a response this host
could not verify — is `Uncertain`. Uncertain is sticky and blocks automatic
*and* explicit retry until `reconcile` records an explicit accepted/rejected
decision. A process that stops mid-flight reopens its claimed receipt as
`Uncertain` on the next open, never as retryable. Receipts are pruned by
capacity and age; tombstones never are, so a duplicate is still recognized as
already-effective long after its receipt has aged out.

`providerRequestId` is derived deterministically from the admitted intent
(contract, mutation, scope, idempotency key, payload digest), so every attempt
presents the provider with the same request identity while the receipt
`attempt` counter advances.

The native bridge now exposes `CursorCloudAdapter` behind the
`ExternalWorkerAdapter` trait. It targets Cursor Cloud Agents API v1, keeps the
API key in the trusted process, requires an explicit repository allowlist, sends
only an isolated exact-repository/ref request, verifies the returned agent/run
identity and write/PR safety flags, polls status, rejects unknown or busy
follow-up targets, enforces terminal cancellation, and refuses to publish
provider artifact listings without both a content digest and run attribution.
Its in-tree fake API fixture covers the bounded checks without credentials.
This is an implementation seam and contract fixture, not evidence that a live
Cursor account has been exercised.

For the v1 request shape, an explicit `repos` entry selects the hosted cloud
environment; a named `env` is not sent alongside `repos` because Cursor treats
those fields as mutually exclusive. The live API's artifact listing currently
returns agent-scoped paths and sizes rather than a digest or run attribution,
so the adapter deliberately fails closed until a trusted download-and-hash
path is qualified.

## Safety contract

1. Keep the Cursor API key in the native/server credential boundary. It must
   never reach the browser, Tauri webview, prompt transcript, or public broker.
2. Require an explicit repository allowlist, exact starting ref, execution
   mode, model/profile, prompt bounds, an idempotency key, and a live
   host-minted scope-bound admission before creation. A mutation without a
   revalidated admission is never sent.
3. Default to isolated cloud work and `autoCreatePR: false`; creating a PR,
   promoting changes, or merging remains a separate human-approved action.
4. Store provider, agent ID, run ID, repo/ref, model, status, timestamps,
   branch/PR metadata, usage, and audit outcome. Redact raw tool output and
   provider credentials from browser projections.
5. Use the stream while it is available, but fall back to the run-status
   endpoint when Cursor reports an expired stream. A closed stream is never
   evidence of completion.
6. Treat Cursor cancellation as terminal. A new run may continue the same
   agent conversation only after an explicit user or policy decision.
7. Allow artifact download only for paths returned by Cursor and only through
   a bounded, audited broker route.
8. Apply the same GrokPtah lease, stale-revision, approval, discard, and
   promotion rules used for local runs. A Cursor agent must not acquire native
   Computer Use authority through this adapter.

## Qualification stages

1. **Contract fixture:** the native fake Cursor API fixture now proves
   isolated create, explicit allowlist and response-safety checks, exact source
   projection, status polling, busy/unknown follow-up rejection, redacted
   terminal text, terminal cancellation, and run-attributed digest-bearing
   artifacts without network credentials. Synthetic authority tests additionally
   prove admission minting and revalidation, scope and expiry binding,
   duplicate launch/follow-up/cancel refusal, restart reconciliation,
   tombstone survival across receipt pruning, uncertain-after-send blocking,
   adapter reachability and version gates, registration collision, redaction,
   and projection-schema agreement. Provider list/archive, stream
   reconnect/expiry, and a real artifact download/hash path remain to be added.

   The fixture also caught a real defect: the adapter's response DTO derived
   `autoCreatePr` from `rename_all = "camelCase"` while Cursor's payloads spell
   the field `autoCreatePR`, so the PR-safety proof could never be satisfied
   and every provider response failed closed. The DTO now accepts both
   spellings and still requires an explicit `false`.
2. **Read-only live probe:** list models/repositories and read an existing
   disposable agent; record API version, limits, retention, and redaction.
3. **Disposable create:** create one agent from an exact public test ref with
   no PR creation, capture branch/status/artifact/usage evidence, then archive
   it and verify no source checkout was changed.
4. **Manager integration:** expose Cursor as a provider-neutral lane in the
   desktop and browser-safe broker, with explicit user-visible provider and
   cost/usage labels. The staged browser client now has typed launch, status,
   artifact, and cancellation calls, and every mutation carries a host-minted
   admission and returns a redacted receipt. The trusted native lifecycle
   adapter and its authority gate are staged, while broker server routes and
   live qualification remain to be completed.
5. **Release gate:** independently review the adapter, run a retry/restart
   soak, and prove that approval, promotion, and Computer Use remain separate.

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
