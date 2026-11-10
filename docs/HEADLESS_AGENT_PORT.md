# Headless agent port

**Schema:** `grokptah.headless-port.v1`
**Protocol version:** `1` (`HEADLESS_PORT_PROTOCOL_VERSION`)
**Module:** `grokptah-agent-bridge::headless_port`
**Status:** internal embedding surface; not a published SDK (ADR-002 §7)

The headless agent port is the host-neutral surface another product — ContextDesk
or any other embedder — uses to drive an existing GrokPtah agent runtime. It adds
no execution path, no persistence, and no provider behaviour. Every effect is
delegated to the orchestration runtime that already ships; the port owns protocol
discipline only.

It is not an MCP surface, an HTTP API, or a desktop integration. Those hosts
remain what they are; the port is a Rust seam an embedder links against, and its
production adapter drives the same `OrchestrationService` methods the desktop and
`grokptah-service` control planes already use.

## Operations

Exactly four. There is no fifth, and none of them is a general query surface.

| Operation | Kind | Tier floor | Returns |
| --- | --- | --- | --- |
| `submit` | mutation | Coordinator | `PortSubmitView` — delivery receipt + run projection |
| `events` | read | Observer | `PortEventsView` — bounded classified page + run projection |
| `review` | read | Observer | `PortReviewProjection` — promotion state, fingerprints, counts |
| `cancel` | mutation | Worker | `PortCancelView` — delivery receipt + run projection |

`events` returns the authoritative run projection alongside the page, so a poller
never reconciles a page against a separately fetched state.

A submit chooses its execution mode. `shared` is the default; `isolated_worktree`
is what makes a run reviewable, because only an isolated run produces a diff for
`review` to describe. `review` on a shared run — or on one that has not completed
and reached a ready promotion state — fails with `conflict`: there is nothing to
review, which is deliberately distinct from lacking scope.

## Bind once, renegotiate always

A `PortBinding` names five things exactly: principal, session, workspace identity,
host, and capability revision. It can only be minted from a completed
`HostNegotiation`, so an embedder cannot fabricate a host identity or a capability
revision.

```rust
let negotiation = port.negotiate(&principal).await?;
let binding = PortBinding::bind(&negotiation, principal, session_id, workspace)?;
```

Every operation then re-reads the host's declared identity, capabilities, and
limits **before** doing anything else. If the host id, the protocol version, or
the capability revision moved, the operation fails closed with `stale_binding` and
the embedder must renegotiate and rebind. Limits are always taken from the fresh
negotiation, never from bind time, so a host that shrinks a limit takes effect on
the next call rather than at the next rebind.

The reference adapter derives `capability_revision` from a hash of the declared
capabilities and limits, so the revision cannot fail to change when the contract
does. A host that tracks its own counter must bump it on every capability or limit
change.

`workspace` is compared as an exact string. The port never touches the filesystem;
canonicalization and the allowlist gate belong to the host adapter.

## Authorization is rechecked at the effect boundary

Negotiation is not authorization. Immediately before a durable effect the port
calls `HeadlessAuthority::authorize_effect`, which re-runs the host's own scope
gate and returns a one-use `EffectAuthorization`. The effect methods consume that
value, and the port cannot construct one — an effect without a live recheck does
not type-check. The port additionally verifies that the issued authorization
describes the same principal, session, workspace, capability revision, and
operation it is about to be spent on.

Authority withdrawn between negotiation and the effect therefore stops the effect,
with nothing performed.

## Durable delivery: write ahead, act, log, acknowledge

Mutations are idempotent on a caller-supplied `request_id` and carry a visible
durable delivery state. The port classifies that state from the host's existing
write-ahead idempotency ledger; it never performs or replays an effect in order to
find out what happened.

| Delivery | Durable evidence | Retry with the same request id |
| --- | --- | --- |
| `unknown` | no claim, no attributable run | yes — this is the only state that proceeds to an effect |
| `sending` | claim written in this host generation, unsettled | no |
| `uncertain` | claim from a previous generation, a claim settled as interrupted, or a run attributable to a request id whose claim never acknowledged it | **no — mint a new request id** |
| `delivered` | claim settled with a durable receipt | no — the receipt is replayed |
| `rejected` | claim settled as a typed refusal with no attributable run | no — the refusal is replayed |

`uncertain` is the important one: the effect **may or may not** have landed. The
port reports it, keeps any attributable run visible, and never resends. A refusal
that nonetheless produced a run is reported as `uncertain`, not as a refusal.

`sending`, `uncertain`, and `rejected` come back as **receipts, not errors**, so an
embedder that only inspects the error path cannot lose them. An `Err` from a
mutation means the request never became a durable claim.

A request id belongs to one operation and, for a cancel, to one run. Reusing a
submit's request id to cancel, reusing a cancel's request id to submit, or
re-pointing a cancel's request id at a different run all fail with `conflict`
rather than replaying a receipt that answers a different question.

## Typed evidence for terminal completion

A completed run is presented as `completed_verified` only when durable typed
evidence supports it. Otherwise the projection says `completed_unverified` and
names the gaps in `evidenceGaps`:

| Gap | Meaning |
| --- | --- |
| `missing_verification` | the run carries no durable completion evidence |
| `unverified_verification` | evidence exists but the runtime did not classify it verified |
| `incomplete_usage` | token accounting is not complete for every attributable request |
| `pending_provider_attempts` | provider attempts were admitted but never reconciled with a response |
| `missing_event_range` | the run has no durable event range, so its timeline cannot be replayed |

The port never upgrades a model claim into a verified completion, and it never
invents evidence. Gaps are evaluated for terminal runs only.

## Principal-scoped reads

The binding is the authorization identity. Unknown run, another session's run,
another workspace's run, a run whose review does not match it, an empty id, and a
traversal-shaped id all produce one **identical** `forbidden_scope` failure, so a
scoped read cannot be used as an existence oracle.

Tiers follow ADR-002 §5 and may only narrow through `PortPrincipal::delegate`:

| Tier | `submit` | `cancel` | `events` | `review` |
| --- | --- | --- | --- | --- |
| `local_operator` | yes | yes | yes | yes |
| `coordinator` | yes | yes | yes | yes |
| `worker` | no | yes | yes | yes |
| `observer` | no | no | yes | yes |

The effective permission is the intersection of the tier row above and the host's
declared capabilities. An operation the tier forbids is `forbidden_scope`; one the
host does not declare at the negotiated revision is `unsupported`.

The port does not authenticate. The host does, and the adapter refuses a binding
whose principal does not match the credential the host already authenticated.

## Cursors and pages

* Pages are clamped to the negotiated `maxEventPage` and to the absolute ceiling
  `MAX_PORT_EVENT_PAGE` (500). `applied_limit` reports what was used.
* Sequences within a page strictly increase and are all greater than the requested
  `after_seq`, and within the run's durable range. A host page that violates this
  is rejected as `internal` rather than passed through.
* `nextCursor` is present only while more entries may remain, and always equals
  the last sequence the page returned — resuming from it skips nothing.
* A cursor below the retained journal window sets `cursorExpired` on an **empty**
  page with no `nextCursor`. A gap is reported; it is never presented as a
  complete stream.

## Redaction contract

Redaction is a property of the shapes, not of a transport filter. Prompts, model
output, filesystem paths, tool input and output, shell commands, credentials, and
provider payloads have **no field anywhere** in the public projections that could
hold them:

* `PortEventKind` is a closed set of **unit** variants. There is nowhere to put
  text, so an event page cannot carry content.
* `PortRunProjection` carries ids, closed enums, counts, limits, timestamps, and a
  sequence range — no prompt preview, no final response, no workspace, no paths.
* `PortReviewProjection` is a decision surface: promotion state, fingerprints,
  changed-file **count**, and whether a diff exists. Diff bytes and changed paths
  stay on the host.
* `PortError::new` accepts only `&'static str`, so a provider body, a model
  string, or a path cannot become an error message. The adapter maps runtime
  errors to fixed text and drops the runtime message.
* `PortSubmitRequest` deliberately does not implement `Serialize`: the prompt is
  input only.

`PortRunFacts` — what a host adapter hands the projection layer — is already
redaction-safe for the same reason, so the mapping in an adapter is the single
place content is dropped, and it cannot be re-added downstream.

## Clock determinism

`project_run_at(facts, delivery, now)` takes an explicit instant. Given the same
`(facts, delivery, now)`, every surface serializes identically. Only `ageMillis`
is clock-derived; every other field is durable. Two live calls that do not share
an instant are not promised to agree on `ageMillis`.

## Error codes

`unauthenticated`, `forbidden_scope`, `stale_binding`, `unsupported`,
`limit_exceeded`, `invalid_request`, `cursor_expired`, `uncertain`, `conflict`,
`unavailable`, `internal`.

## Embedding

```rust
use grokptah_agent_bridge::{
    orchestration_port, PortBinding, PortHostKind, PortPrincipal, PortSubmitRequest, PortTier,
};

let port = orchestration_port(service, auth, "service-1", PortHostKind::Service, started_at)?;
let principal = PortPrincipal::new(owner_id, credential_id, PortTier::Coordinator)?;
let negotiation = port.negotiate(&principal).await?;
let binding = PortBinding::bind(&negotiation, principal, session_id, workspace)?;

let request = PortSubmitRequest::new(request_id, prompt)?
    .with_execution_mode(PortExecutionMode::IsolatedWorktree);
let view = port.submit(&binding, &request, Utc::now()).await?;
match view.receipt.delivery {
    PortDelivery::Delivered => { /* poll `events` from cursor 0 */ }
    PortDelivery::Uncertain => { /* surface it; mint a new request id, never resend */ }
    other => { /* `sending` / `rejected`: report, do not retry this request id */ }
}
```

`OrchestrationAuthority` is constructed with an already-authenticated
`AuthContext`; `generation_started_at` must be the instant this host process
generation began serving, because that is what separates `sending` from
`uncertain` after a restart.

## Versioning

`HEADLESS_PORT_PROTOCOL_VERSION` covers the whole surface: operations, binding
rules, delivery semantics, projection shapes, and cursor behaviour. A host that
negotiates a different version is refused rather than adapted — there is no
partial-compatibility mode.

Changes that require a version bump:

1. adding, removing, or renaming an operation;
2. adding a field that can carry content to any public projection;
3. changing a delivery classification rule, a cursor rule, or the terminal-evidence
   rule;
4. removing a variant from any closed enum, or giving one a payload.

Adding a new closed-enum variant is a compatible change **only** if consumers are
required to treat unrecognized variants as fail-closed. `PortEventKind` reserves
`unclassified` for exactly that.

Capability revisions are orthogonal: they change whenever a host's declared
capabilities or limits change, within one protocol version.

## Verify

```sh
cargo test --locked --manifest-path crates/codegen/grokptah-agent-bridge/Cargo.toml \
  --lib headless_port -- --test-threads=1
```

The suite is deterministic: a fake host supplies every instant, and no test
touches a clock, a filesystem, a provider, or a network. It covers restart,
stale revision, wrong scope, page gaps, uncertain send, cancel, redaction, and
small/large-model bounds, plus tier narrowing, effect-boundary revocation,
authorization mismatch, undeclared capability, and the structural assertions that
the port owns no send engine and its core names no host.

| Scenario | Guarantee |
| --- | --- |
| Restart | An unsettled claim becomes `uncertain`, its run stays visible as `interrupted`, and nothing is replayed. A new request id is required and does send. |
| Stale revision | A changed limit changes the capability revision; all four operations fail closed as `stale_binding` before reaching an effect. |
| Wrong scope | Cross-session, cross-workspace, unknown, empty, and traversal-shaped resources produce one byte-identical `forbidden_scope`. |
| Page gaps | An expired cursor returns an empty page with `cursorExpired`; a retained cursor resumes exactly; pages clamp to the negotiated limit. |
| Uncertain send | An effect that landed without an acknowledgement is reported `uncertain` and never re-sent; a run with no acknowledging claim is `uncertain` too. |
| Cancel | Cancel is a durable effect with the same discipline; a repeated request id replays the receipt without cancelling twice; an out-of-scope run never reaches the effect. |
| Redaction | Prompt, path, credential, and model-output markers are absent from every serialized projection, and the projection key sets are asserted exactly. |
| Small/large-model bounds | An oversized prompt is refused under small limits with nothing performed, admitted after the host grows them; a caller may narrow bounds but never widen them. |
| Execution mode | A submit that does not ask for isolation does not get it; a reviewable run must request `isolated_worktree` explicitly. |
| Request-id reuse | A request id reused across operations, or re-pointed at another run, is a `conflict` rather than a replay. |

## Non-goals

No Computer Use, no desktop UI, no provider wire, no second send engine, no new
durable store, no MCP tool surface, and no published crate. Per ADR-002 §7 this
stays an internal module until a second named consumer exercises it in running
code; ContextDesk counts as that consumer only once it does.
