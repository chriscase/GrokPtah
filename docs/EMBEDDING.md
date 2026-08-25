# Embedding GrokPtah in another product

GrokPtah is designed to be consumed by more than its own Tauri desktop. The
stable direction is one capability-scoped contract with separate adapters for
trusted desktop hosts and browser-facing brokers. The examples below use the
current in-tree staging surfaces or their generated package equivalents; they
are not a claim that a public npm or crates.io package has been released yet.

## Choose the trust boundary first

| Consumer | Import surface | Credential boundary | Allowed default |
| --- | --- | --- | --- |
| Desktop host or trusted local adapter | `desktop/src/lib/trusted.ts` | Host keeps the loopback/MCP credential | Observe, execute, review; promotion and Computer Use remain human-gated |
| Browser or War Room UI | `@grokptah/client` (generated from `desktop/src/lib/public.ts`) | Cookie/session broker; no GrokPtah bearer token | Observe, review, and broker-approved bounded runs |
| Rust desktop/server adapter | `crates/common/grokptah-agent-sdk` | Host adapter owns credentials and policy | Versioned DTOs, validation, replay, review, and lease contracts |

Never import `trusted.ts` into a browser bundle. A browser must not connect to
the loopback MCP endpoint, receive a GrokPtah token, or provide raw workspace
paths and Computer Use details.

## External cloud workers

Products that want GrokPtah to launch and monitor an external coding agent
should import the `external_worker` contracts from `grokptah-agent-sdk` rather
than inventing provider-specific DTOs. The contract covers an exact-ref,
isolated launch request, opaque worker/run identities, redacted event updates,
and bounded relative artifact references. The provider adapter keeps its API
key and network client in the trusted service; the desktop and War Room UI
receive only the redacted projections and existing review/approval receipts.

The JSON representation is versioned at
[`grokptah-external-worker.v1.schema.json`](./schemas/grokptah-external-worker.v1.schema.json).
The initial Cursor Cloud adapter remains a qualification task; the contract is
already reusable by ContextDesk, another Rust service, or a future TypeScript
broker adapter.

## Browser / War Room example

The browser receives only an authenticated broker session and opaque ids:

```ts
// After `npm run verify:public`, import from the generated package root.
import { GrokPtahBrokerClient } from "@grokptah/client";

const grokptah = new GrokPtahBrokerClient({
  baseUrl: "https://contextdesk.example",
  // The broker session is carried by cookies; this is not a GrokPtah token.
  credentials: "include",
  csrfToken: brokerCsrfToken,
});

const binding = await grokptah.createBinding(
  investigationId,
  approvedWorkspaceAlias,
  ["session.observe", "run.execute", "run.review"],
  crypto.randomUUID(),
);

const run = await grokptah.submitRun(
  binding.bindingId,
  {
    prompt: "Review the staged change for correctness and security.",
    executionMode: "isolated_worktree",
    bounds: { maxRounds: 12, maxDurationMs: 1_800_000 },
    allowQueue: true,
  },
  crypto.randomUUID(),
);

for await (const notification of grokptah.streamEvents(binding.bindingId, run.brokerRunId)) {
  if (notification.kind === "recovery") {
    // Poll the broker's advertised relative route, then reconnect from its cursor.
    break;
  }
  renderRedactedProgress(notification.update);
}
```

The client validates opaque binding/run response envelopes and bounded run,
queue, and steer requests, requires CSRF and idempotency headers for
mutations, rejects external recovery URLs, and enforces monotonic event
sequence numbers. It also validates every streamed event update through the
browser-safe `parseBrokerEventUpdate` contract before exposing it to a UI;
unknown fields, impossible round bounds, path-like text, URLs, and privileged
needles fail closed. The broker remains authoritative: it must
re-check the user, team, workspace, capability, policy, and exact run scope.

For a typed War Room status/evidence surface, prefer
`getRunProjection(bindingId, brokerRunId)` and
`getReviewProjection(bindingId, brokerRunId)`. These opt-in methods accept only
the redacted run state/progress envelope and bounded review receipt shape;
unknown fields, path-like data, oversized diffs, invalid state transitions, and
scope mismatches fail closed. The older generic `getRun<T>` and `getReview<T>`
methods remain available for forward-compatible integrations, but they should
not be used as authority-bearing UI data without an equivalent server-side
parser.

## Trusted desktop adapter example

The trusted path may use the direct MCP client or the typed operation facade,
but it must keep the credential outside the webview and bind every call to an
explicit session/workspace/run identity. A desktop adapter should:

1. Discover and authenticate to the local authority without exposing its token.
2. Negotiate the versioned capability set and reject unknown contracts.
3. Use durable cursors for reconnect and treat uncertain delivery as unknown.
4. Present promotion and Computer Use as separate human-visible gates.
5. Fail closed on stale revisions, expired leases, locked hosts, and cleanup uncertainty.

## Headless UI primitives

Products that want their own visual language can use the Tauri-free
`@grokptah/client/ui-core` entry (generated from `desktop/src/lib/uiCore.ts`). It exposes capability negotiation,
the source-cited Help Center corpus, prompt-queue
reducers, and stream application helpers, but
no React components, native APIs, credentials, or desktop state:

```ts
import {
  applyAssistantStreamChunk,
  promptQueueReducer,
  searchHelp,
} from "@grokptah/client/ui-core";
```

### One Help corpus

Browser consumers see exactly one Help corpus: the live, source-cited
`product-corpus-v1` corpus the desktop actually renders. `searchHelp` and
`searchHelpArticles` are the **same binding**, so neither name can drift onto a
different corpus.

The capability-gated `grokptah.help.v1` corpus (`HELP_ENTRIES`, `HELP_CONTRACT`,
`buildHelpAssistantContext`) carries `audience` / `access` / `capabilityIds`
metadata and defaults to public-only results. It remains the contract for
trusted adapters and is reachable only through `desktop/src/lib/trusted.ts`,
which is never shipped in a browser bundle. It is not exported from
`@grokptah/client` or `@grokptah/client/ui-core`, and `verify:public` fails if
it reappears there.

The reducer inputs and stream cursors remain host-neutral. A consumer owns its
rendering, focus management, transport adapter, and approval presentation.

### Shared visual layer — and what it does not cover

`@grokptah/client/styles/tokens.css` is the whole shared visual layer. It is
staged verbatim from the marked regions of `desktop/src/styles/app.css` at
build time, so the desktop and a browser consumer cannot drift apart:

```ts
import "@grokptah/client/styles/tokens.css";
```

It contains colour tokens for both themes and all three accents, the type scale
with its density and type-scale controls, the focus indicator, the
`prefers-contrast` and `forced-colors` escapes, and the `.sr-only` utility.

It is **not** a component library. Continuity that may be claimed on this tree
is contract-level plus these tokens:

| May be claimed | May not be claimed |
|---|---|
| Help corpus, prompt-queue reducers, stream-apply helpers, external-worker parsers, capability types | Component or layout parity |
| Colour, type-scale, focus, contrast and screen-reader tokens | Focus management, focus trapping, inert handling, live regions |
| Both themes and all three accent palettes | Packaged screen-reader, high-zoom or forced-colour acceptance |

Every focus trap, inert background, live region and approval presentation in
the desktop app is desktop-only. A browser consumer implements its own, and
nothing in this package does it for them.
The generated package now exposes the same headless surface as the
`@grokptah/client/ui-core` subpath. It is still a staging artifact rather than
a published SemVer promise, but consumers can exercise the real import path
before publication. The same subpath now exposes the external-worker parsers
and cursor-aware monitor reducer, so a War Room can render cloud-worker status
without importing React, Tauri, credentials, or provider-specific response
shapes.

To produce a reviewable, Tauri-free consumer artifact from an exact checkout,
run `npm run verify:public` in `desktop/`. This builds and verifies a bundled
`desktop/dist/public/grokptah-public.js`, `desktop/dist/public/ui-core.js`,
declaration files under `desktop/dist/public/types/`, the derived
`desktop/dist/public/styles/tokens.css`, and a package manifest
for `@grokptah/client` with its `./ui-core` and `./styles/tokens.css` subpaths.
The generated artifact contains the browser-safe broker client and headless
primitives only; it does not contain `trusted.ts`, Tauri APIs, bearer tokens,
provider API-key markers, absolute host paths, `GROKPTAH_HOME`, or native
Computer Use authority. The same command then installs that generated manifest
into a disposable external-consumer fixture, installs the generated `npm pack`
archive, and imports it through normal `node_modules/@grokptah/client` package
resolution. That fixture exercises the
Help Center corpus, queue reducer, stream helper, broker constructor, the
separate `@grokptah/client/ui-core` import, and the `styles/tokens.css`
subpath, so a direct bundle import cannot
masquerade as a consumer integration. The fixture is deleted after the check.
Publication still requires the compatibility and release gates in the roadmap.

## ContextDesk integration checklist

The minimum disposable integration should prove, against one exact candidate:

- binding creation and capability negotiation;
- bounded submit, progress, handoff, changes, and tests;
- SSE reconnect from a cursor and cursor-expiry recovery;
- isolated review, exact approval evidence, and safe discard;
- duplicate request-id behavior and restart recovery;
- no browser access to bearer tokens, raw paths, or native Computer Use data;
- audit records for user, capability, scope, request id, and outcome.

Keep the War Room surface observe/review-only until packaged Computer Use,
lease soak, and human acceptance evidence are green. Promotion is a separate
short-lived approval flow; it is never inferred from a login, focused tab, or
transport reachability.

## Publishing path

The eventual published surfaces should be generated from the same contracts:

- `@grokptah/client`: typed browser-safe broker and host-neutral DTOs;
- `@grokptah/ui-core`: headless state machines and hooks — accessibility
  *behavior* (focus traps, inert handling, live regions) is not part of the
  surface today and must not be claimed until it ships;
- `@grokptah/ui`: optional styled components on top of the already-shipped
  `styles/tokens.css`;
- `grokptah-agent-sdk`: versioned Rust DTOs and validation vocabulary.

Publish only after the compatibility, schema migration, security, Always-On,
gateway, packaged Computer Use, and ContextDesk end-to-end gates in
[`ROADMAP_TO_100.md`](./ROADMAP_TO_100.md) are green.
