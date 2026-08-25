# ADR-003: Cross-product capability surface

**Status:** In progress — contract and package staging implemented; consumer qualification open
**Date:** 2026-08-24
**Scope:** GrokPtah consumers such as ContextDesk desktop and War Room

## Decision in one sentence

Expose GrokPtah through one versioned, capability-scoped contract surface and
provide separate desktop and web-broker adapters; consumer products must not
import GrokPtah's desktop state or reach its credentials directly.

## Why this is now a real boundary

GrokPtah already has an authenticated loopback MCP control plane for sessions,
runs, queues, isolated review/promotion, durable events, and redacted Computer
Use projections. ContextDesk now has an independent host-neutral SDK/runtime
pattern and needs to consume coding-agent capabilities from both its Tauri
desktop and War Room web UI. This is the second-consumer trigger described by
ADR-002, not a speculative abstraction.

The existing control plane remains useful as the first transport. It is not,
by itself, the long-term public SDK: its current authority is a local desktop
process, its token is loopback-scoped, and its wire inventory is broader than
the stable contract we want third-party products to depend on.

## Consumers and adapters

```text
                 +-------------------------------+
                 | grokptah-agent-sdk             |
                 | versioned DTOs, capabilities,  |
                 | events, errors, policy tiers   |
                 +---------------+---------------+
                                 |
          +----------------------+----------------------+
          |                                             |
  +-------v--------+                             +------v-------+
  | Desktop adapter |                             | Web broker    |
  | Tauri / local   |                             | ContextDesk   |
  | authority       |                             | server        |
  +-------+--------+                             +------+-------+
          |                                             |
  +-------v--------+                             +------v-------+
  | GrokPtah bridge |                             | scoped client |
  | + local MCP     |                             | + SSE/WebSocket|
  +-----------------+                             +--------------+
```

The web broker is an authority boundary, not a tunnel for a desktop bearer
token. A browser receives only a broker session and capability-scoped events.
The broker authenticates the user, maps the request to an explicitly approved
workspace/run, and asks the local GrokPtah authority to perform the operation.

### External agent workers

The agentic harness may also schedule external cloud workers behind the same
provider-neutral run contract. The first concrete candidate is Cursor Cloud's
official Cloud Agents API: GrokPtah can create an isolated agent from an exact
Git ref, follow up, stream and poll status, cancel, collect bounded artifacts,
and archive the worker. The API key remains server-side; a browser sees only
the same redacted run projections, review receipts, and approval gates used for
local runs. This is a planned adapter, not a claim that Cursor Cloud is
currently integrated or qualified. The reusable launch/status/event/artifact
DTOs now live in `grokptah-agent-sdk::external_worker` and have a matching
versioned JSON Schema. A trusted native Cursor Cloud v1 adapter, host
allowlist, and durable launch/follow-up/cancel ledger are staged behind a
fake-API fixture; streaming is explicitly unsupported (`lastSeq` must stay
null), live artifact listings without run attribution fail closed, and live
qualification remains open. This is not a 100% claim.
See [`CURSOR_CLOUD_INTEGRATION.md`](./CURSOR_CLOUD_INTEGRATION.md).

## Public contract layers

### 1. `grokptah-agent-sdk` (Rust, stable contract)

This crate should contain no Tauri, keychain, filesystem, provider SDK, or
platform-native dependencies. It owns versioned, serializable contracts for:

- capability discovery and negotiated contract version;
- sessions, Build runs, durable run state, handoffs, and bounded progress;
- queue/steer/cancel operations with request IDs and compare-and-set versions;
- isolated worktree review, approval, promotion, and discard receipts;
- provider/gateway profile identity and capability qualification results;
- typed event pages, cursors, replay/recovery, and terminal outcomes;
- Computer Use run projections and bounded control requests;
- redaction, privacy, and error categories.

The crate must not contain execution policy that requires the desktop. A host
adapter implements the execution port and retains authority over credentials,
workspace allowlists, permissions, PTYs, and Computer Use grants.

### 2. Wire schema and non-Rust clients

The same contract must be representable as JSON Schema (or an equivalent
generated schema set) so ContextDesk's Rust, TypeScript, and future clients do
not reimplement GrokPtah DTOs independently. Schema IDs are versioned, for
example `grokptah.run.v1` and `grokptah.computer_run_projection.v1`.

The existing MCP Streamable HTTP transport can carry v1 operations while the
contract is stabilized. A small `@grokptah/client` package should later expose
typed TypeScript methods and cursor-aware event streams without exposing
tokens, raw prompts, or arbitrary desktop state.

The Tauri-free source barrels are split by trust boundary. Browser consumers
use `desktop/src/lib/public.ts`, which exports only the broker client,
capability contracts, and help index. A trusted desktop/server adapter may use
`desktop/src/lib/trusted.ts`, which contains the direct MCP client and therefore
must never be shipped in a browser bundle. The public surface now has a
reproducible `@grokptah/client` package staging build and consumer smoke check;
the generated package remains a release candidate until SemVer compatibility
and cross-product qualification are complete.

The project-wide ordered status and 100% exit gate are tracked in
[`docs/ROADMAP_TO_100.md`](./ROADMAP_TO_100.md).

Provider/product boundaries, including the explicit separation between Grok
Build evidence and the unrelated Grok Bot product, are recorded in
[`PROVIDER_PRODUCT_BOUNDARIES.md`](./PROVIDER_PRODUCT_BOUNDARIES.md).

The consumer-facing staging guide is [`docs/EMBEDDING.md`](./EMBEDDING.md).
It documents the trust-boundary choice and the disposable ContextDesk
desktop/War Room integration sequence without presenting the staging barrels
as published compatibility promises.

### 3. UI packages

UI reuse is intentionally split:

- `@grokptah/ui-core`: headless React hooks, capability negotiation, event
  reducers, queue/run state machines, and accessibility behavior;
- `@grokptah/ui`: optional styled components and theme tokens used by the
  GrokPtah desktop;
- ContextDesk may use the headless layer with its own War Room visual language.

Neither package may import Tauri APIs. Desktop-only behavior belongs in a
small adapter supplied by the host application.

## Capability tiers

Every client obtains an explicit capability set. The minimum tiers are:

| Tier | Examples | Default authority |
| --- | --- | --- |
| Observe | sessions, capacity, progress, handoff, redacted events | read-only client |
| Execute | submit, retry, queue, steer, cancel | approved client + workspace scope |
| Review | changes, tests, isolated-run review, fingerprints | human/operator review |
| Promote | approve/promote/discard an isolated run | desktop authority + human gate |
| Computer observe | scoped projections and audit events | desktop authority, redacted |
| Computer control | semantic actions within a leased run | explicit grant, lease, revision, expiry |

Promotion and Computer Use grants are never inferred from a web login, a
focused browser tab, or possession of a read token.

## Desktop path

The GrokPtah Tauri app remains the local authority anchor. ContextDesk desktop
may connect through a local adapter using the authenticated MCP endpoint or a
future in-process Rust adapter. The adapter must:

1. discover the endpoint without exposing the bearer token to the webview;
2. negotiate the contract and capability set;
3. bind every operation to an explicit session/workspace/run identity;
4. replay events from a durable cursor after reconnect;
5. present promotion and Computer Use requests as human-visible approvals;
6. fail closed when the host is asleep, locked, stopped, or out of scope.

Independent review of this boundary follows
[`docs/INDEPENDENT_REVIEW_PROTOCOL.md`](./INDEPENDENT_REVIEW_PROTOCOL.md): a
separate strongest-model Cursor/Claude lane, exact-head pin, Fast off,
read-only scope, and evidence for every authority and replay invariant.

## War Room web path

The browser must not connect directly to GrokPtah's loopback endpoint. The
ContextDesk server (or another trusted local broker) owns:

- user/team authentication and authorization;
- mapping from War Room investigation to approved GrokPtah workspace/run;
- server-side storage of broker credentials, never browser storage;
- redacted event fan-out with bounded replay cursors;
- approval UX for review, promotion, and Computer Use grants;
- audit records linking user, capability, request ID, and outcome.

The broker may expose a WebSocket or SSE stream to the browser, but each frame
must preserve the GrokPtah run identity, sequence, and recovery semantics.
Browser disconnects must be recoverable by cursor, not treated as run loss.

## Required implementation order

1. Freeze the v1 capability vocabulary and error/redaction taxonomy from the
   current MCP control plane.
2. Extract the transport-neutral Rust contract crate and add conformance tests.
3. Make the current desktop bridge implement the contract without changing
   authority or persistence semantics.
4. Add a typed TypeScript client for the same schema and a ContextDesk adapter
   against a disposable local GrokPtah instance.
5. Add the broker protocol and audit/approval path for War Room use.
6. Extract headless UI reducers/hooks, then optional styled components.
7. Publish packages only after compatibility, security, soak, and packaged
   Computer Use gates pass.

### Current implementation checkpoint (2026-08-24)

The first consumer-facing pieces now exist in the GrokPtah tree without
changing the authority boundary:

- `crates/common/grokptah-agent-sdk` is a host-neutral Rust contract crate for
  capabilities, scopes, runs, replay, review receipts, errors, and Computer
  Use leases. It has no Tauri, provider, filesystem, network, or credential
  dependency.
- The desktop bridge now depends on that crate for capability DTOs; only its
  allowlist-derived advertisement policy remains bridge-owned.
- `desktop/src/lib/capabilities.ts` parses the versioned capability payload and
  fails closed on unknown contracts or malformed descriptors.
- `desktop/src/lib/grokptahClient.ts` provides a Tauri-free MCP handshake,
  capability negotiation, scoped tool calls, and cursor-aware SSE recovery.
- `desktop/src/lib/grokptahOperations.ts` provides typed, scope-fenced helpers
  for sessions, runs, review/promotion, queues, durable agents, and redacted
  Computer Use. Gated operations require an explicit approval flag in addition
  to the server's own authorization.
- `desktop/src/lib/grokptahBrokerClient.ts` is a separate browser-safe shape
  for ContextDesk War Room: it has no bearer-token option, uses the user's
  broker session, accepts only opaque binding/run ids, validates binding/run
  response envelopes before exposing them to consumers, and enforces
  cursor-aware redacted SSE recovery.
- `desktop/src/lib/uiCore.ts` is a headless, Tauri-free surface for
  capability/help search, prompt-queue reducers, and stream application
  helpers. It contains no React components or native authority and is staged
  as the real `@grokptah/client/ui-core` subpath; a separately versioned
  `@grokptah/ui-core` package remains a release decision.
- `docs/schemas/grokptah-run.v1.schema.json` pins the shared run, event,
  recovery, and review-receipt JSON shapes used by non-Rust consumers.
- `docs/WEB_BROKER_PROTOCOL.md` is the concrete server-side contract for
  ContextDesk War Room. It intentionally keeps browser access behind a broker.

These are integration foundations, not a claim that ContextDesk is already
connected or that promotion/Computer Use is safe to expose remotely. The
adapter, broker, cross-language conformance, and qualification gates remain.

## Exit criteria

This ADR is not complete when a type can be imported. It is complete only when
all of the following are evidenced:

- an independent ContextDesk desktop adapter can submit, observe, reconnect,
  review, and safely discard an isolated run;
- a War Room broker can do the same without giving the browser desktop access;
- event loss, cursor expiry, restart recovery, stale versions, and duplicate
  request IDs are covered by cross-language conformance tests;
- promotion and Computer Use remain human-gated and scope-bound;
- the UI packages run outside Tauri in a test host with keyboard and screen
  reader coverage;
- SemVer, schema migration, examples, and release artifacts are published;
- the Always-On, gateway, integration, and packaged Computer Use qualification
  evidence is green.

## Relationship to existing decisions

ADR-002's desktop authority, client tiers, durable-ID rules, and fail-closed
principles remain in force. This ADR records the observed second-consumer
trigger and authorizes extraction of contracts; it does not authorize a remote
originator, unattended Computer Use, auto-promotion, or secret sharing.
