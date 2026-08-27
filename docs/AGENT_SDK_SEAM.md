# GrokPtah agent SDK seam

Status: **contract 1.1 shipped; service adapter shipped; read-only
observatory shipped; desktop adapter and a live two-host run pending.** This document is the design packet and the
implementation guide for `crates/codegen/grokptah-agent-sdk`.

## Why this exists

GrokPtah already has two hosts over one runtime: the Tauri desktop and
`grokptah-service`. A third consumer — ContextDesk, or any other UI — currently
has only two ways in, and both are bad:

1. **Depend on `grokptah-agent-bridge`.** That crate is the runtime. It carries
   keyring access, an Axum control plane, `reqwest`, provider profiles, durable
   stores, and roughly 99k lines of implementation. Depending on it to *talk to*
   an agent host drags all of that into the consumer's build and makes every
   internal type part of the consumer's compatibility surface.
2. **Hand-roll a JSON-RPC client against `ptah_*`.** The MCP contract is real
   and well documented (`docs/MCP_CONTROL_COORDINATOR.md`), but it is untyped at
   the call site. Each consumer re-derives the same DTOs, the same error
   mapping, the same cursor rules, and the same retry policy — and each one gets
   a slightly different subset right.

The SDK is the narrow waist between them: a dependency-light crate that carries
the contract and nothing else.

## What ADR-002 permits, and what it does not

ADR-002 §7 fixes the ordering for reusable behavior:

> 1. place policy and state transitions in the bridge runtime;
> 2. define a versioned MCP/API contract with bounded schemas;
> 3. prove desktop/service parity with a shared, versioned fixture matrix and
>    stated pass criteria against both hosts;
> 4. extract a separate crate only after two named running consumers execute
>    that matrix against the same boundary; and
> 5. publish an SDK or split a repository only when a named compatibility and
>    version owner maintains that matrix for a real external consumer.

Step 1 is done. This change delivers **step 2** and the *matrix* required by
step 3, and it deliberately stops there:

* No behavior moves out of `grokptah-agent-bridge`. Nothing is extracted. The
  bridge is untouched by this change.
* The crate is `publish = false`. Step 5 has not been met.
* The parity matrix (`conformance::run_battery`) exists and is exercised, but
  only against the fake. Running it against desktop and service is step 3's
  remaining half and is tracked below as P1.

ADR-002 §3 also records a required future contract:

> Explicit capability advertisement is a required future contract before
> workload assignment may select a worker by capability. That contract must
> define stable capability identifiers, the host/version that asserted them,
> attempt-time capture, and typed unsupported/forbidden failures.

`capability::CapabilityDocument` is that contract.

## The boundary

One trait, `client::AgentControlPlane`, with the operations a consumer needs:

| Method | Capability | Backing runtime surface | Service adapter |
|---|---|---|---|
| `capabilities` | — | `tools/list` (the host-owned tool registry) | ✅ |
| `create_session` | `session.create` | `ptah_create_session` | ✅ (not idempotent — see below) |
| `list_sessions` | `session.list` | `ptah_list_sessions` | ✅ (adapter-side paging) |
| `submit_task` | `task.submit` | `ptah_submit_task` | ✅ |
| `observe_run` | `run.observe` | `ptah_get_run` alone — the durable record already carries bounds, usage, changes and verification | ✅ |
| `stream_events` | `run.events.page` | `ptah_get_events` | ✅ |
| `request_follow_up` | `run.followup` | `ptah_steer` | ✅ (no fence — see below) |
| `cancel_run` | `run.cancel` | `ptah_cancel` + a re-read | ✅ |
| `acquire_control` / `release_control` | `control.lease` | `ptah_claim_work` / `ptah_release_work` | ✅ |
| `fetch_artifact` | `artifact.fetch` | `ptah_get_test_results` only | ✅ (test report; `ptah_review_run` declined) |

`run.events.live` (the scoped SSE channel and its `ptah_recovery` gap notice) is
declared but not yet in the trait — see P1.

## The service adapter

`service::ServiceControlPlane` implements the trait over the authenticated MCP
control plane. Four properties are worth stating outright.

**It cannot reach a network.** The adapter speaks the domain contract only;
framing lives behind `service::McpTransport`, which an embedder implements over
its own HTTP client. This crate has no HTTP client, so no code path in it can
reach a provider, a gateway, or any route. Every test drives a scripted
transport that emulates the `ptah_*` wire contract.

**It is read-only by default.** `ServiceControlPlane::read_only` is the only
constructor that needs no assertion from the embedder; in that mode every
mutating method fails with `forbidden_scope` *before the transport is touched*,
and the capability document advertises those capabilities as `Forbidden` so a
consumer can grey out the control instead of discovering the refusal on click.
`with_operator_authority` opts in, and is the embedder asserting what ADR-002 §5
already states: possession of a configured service bearer is privileged
operator access. The restriction therefore protects against consumer mistakes,
not against a malicious consumer holding the same bearer.

**Its workspace registry is learned, never declared.** A `WorkspaceRef` exists
only after the host reported that workspace in a `ptah_list_sessions` or
`ptah_create_session` response. A ref the host has not reported resolves to
`workspace_mismatch` without a round trip, matching the runtime, where the
allowlist gate is session-independent and precedes every scope check. Refs are
`ws-` plus 16 hex characters of `SHA-256(key ‖ 0x00 ‖ path)`. With the default
key this obfuscates the path but does not hide it — workspace paths are
low-entropy, so an attacker who can guess one can confirm it against a ref.
`WorkspaceRegistry::with_ref_key` takes a persistent secret for embedders that
need real opacity; host-issued refs remain the durable fix (P1).

**Redaction happens here.** `ptah_get_run` returns the *complete* durable
`RunRecord`: `promptPreview`, `finalResponse`, the absolute `workspace` path,
`requestId`, and `clientId` are all on the wire. None of them survive
`project_run`, because none of them exist on `RunView`. Journal projection drops
message and thought chunks, shell output, tool-call output, and
`FileEdit.unified_diff`; a changed-file path the host reports as absolute is
dropped rather than surfaced. The test-report artifact drops the recorded
`command` string, which can carry absolute paths. Contract tests assert each of
these against fixtures that deliberately contain the secret.

### What the adapter refuses to map

The host's tool surface is much larger than this contract. The adapter calls
exactly ten tools and asserts in debug builds if asked for an eleventh. It
declines, on purpose:

| Declined | Why |
|---|---|
| `ptah_create_manager_plan`, `ptah_advance_manager_plan`, `ptah_tick_manager_plan`, `ptah_replan_manager_plan` | Manager plans are an **active line** — bridge #337, #338 and #339 are the three newest commits on `main`, `MANAGER_SCHEMA_VERSION` is 1, and the surface is still moving. Mapping it now would couple a public contract to a design in flight. |
| `ptah_set_managed_execution`, `ptah_get_managed_execution`, `ptah_authorize_work_execution`, `ptah_resolve_work_input`, `ptah_list_execution_intents` | This is where a **mutation grant** is issued and durably recorded. That authority stays host-owned: a consumer of this seam must not be able to issue one, replay one, or infer that one exists. |
| `ptah_approve_run`, `ptah_promote_run`, `ptah_discard_run`, `ptah_review_run` | Operator authority — reviewing, approving and promoting code. ADR-002 §5 keeps these operator-equivalent, and `ptah_review_run` is entangled with the approval it feeds. |
| `ptah_*_computer_*` | Computer Use reads are redaction-safe but unmapped in this build, and advertised as `Unsupported` rather than silently missing. Computer Use *control* is permanently forbidden. |
| Queue mutators, routines, workers, messages, work lifecycle beyond claim/release | Outside the declared v1 capability set. |

A contract test advertises the manager, managed-execution and promotion tools
from the host double and asserts the adapter never calls one.

`control.lease` (`ptah_claim_work` / `ptah_release_work`) *is* mapped. It sits
adjacent to the manager line — managed execution references the same work
items — but claim and release predate it, carry their own schema version, and
are part of the declared v1 capability set. They are gated behind mutation
authority like every other mutation.

## The read-only observatory (contract 1.1)

`AgentControlPlane` mixes reads and mutations, because a host adapter needs
both. An external consumer usually needs only the reads — and "usually" is not
a security property.

`observe::RunObservatory` contains the read operations and nothing else.
`observe::ObserverHandle` wraps any control plane and implements only that
trait; it deliberately does **not** implement `AgentControlPlane` and exposes
no accessor for the plane it wraps. A consumer holding one cannot submit,
cancel, steer, create, or lease — not because a check refuses, but because the
methods are not there. There is no flag to flip and no downcast to find.

This is strictly stronger than `ServiceControlPlane::read_only`, which enforces
the same restriction at call time on a value that still *has* the mutating
methods. Both are useful, for different reasons:

| | Enforced by | Use it when |
|---|---|---|
| `ServiceControlPlane::read_only` | a runtime check before the transport | one embedder keeps a single plane and wants mutations off |
| `ObserverHandle` | the type system | you are handing a plane across a trust boundary |

### Narrowing is one-way

`ObserverHandle::capabilities` rewrites the host's document before returning
it, so an observer is never told it holds authority it cannot exercise. Two
rules govern the rewrite:

* **Availability only decreases.** A capability the host already reported as
  `Unsupported` keeps that answer. "This host cannot" is more informative than
  "you may not", and it stays true when the handle is unwrapped.
* **An unrecognized capability counts as a mutation.** `CapabilityId::Unknown`
  returns `true` from `is_mutation()`, so a capability a future host invents is
  withheld from an observer until this build knows what it does. Refusing an
  unknown capability is recoverable; granting one is not.

The host's own `contractVersion` is carried through unchanged rather than
restamped with this build's, so a consumer negotiates against what the host
actually said.

### Redacted receipts

`ReceiptView` is evidence that a mutation happened, for a consumer that did not
perform it and may not perform one. It carries the request id, a closed
`OperationClass`, a `ReceiptStatus`, the typed failure code when there is one,
a digest of the payload, and the run it belongs to.

What it does not carry, and why each one matters:

| Absent | Why |
|---|---|
| The stored response body | The runtime replays a mutation's *full response* from its receipt. That body is whatever the mutation returned — prompts, workspace paths, queue entries. |
| The failure message | Runtime messages embed absolute paths verbatim. `canonical_workspace` formats one straight into a `workspace_mismatch` (`orchestration/authz.rs`). The typed `SdkErrorCode` carries the meaning without the text. |
| The raw tool name | Host vocabulary. A closed `OperationClass` means a host that adds a tool cannot put an arbitrary string in front of a consumer that believes it is reading a classification; anything unrecognized is `Other`. |
| The request payload | Only its digest crosses — enough to tell one attempt from another, revealing neither. |

Receipts are **run-scoped by construction**: `list_receipts` takes a full
`RunSelector`, there is no global listing, and an out-of-scope run returns the
same `forbidden_scope` every other read gives. A mutation with no run — a
session creation, a lease — is simply not listed.

`ReceiptStatus::Pending` is the uncertain-send fence in durable form: the host
claimed the key and stopped before recording an outcome, so the effect is
unknown. `ReceiptView::is_uncertain()` names it, and the projection asserts no
`outcome` in either direction. An observer must not report such a mutation as
applied or as refused.

### What serves it today

| Adapter | `receipt.read` |
|---|---|
| `FakeControlPlane` | `Available` — served from its durable receipt ledger |
| `ServiceControlPlane` | `Unsupported`, with the reason |
| any adapter written against 1.0 | the trait's default body returns `capability_unavailable` |

The service adapter cannot serve receipts because **the control plane exposes
no receipt, audit, or idempotency read** — zero of its 89 `ptah_*` tool names
match. The durable receipts exist; nothing reads them. Reporting `unsupported`
is the honest answer, and an empty page would be the dishonest one: a consumer
would read it as "no mutations happened".

Closing that gap is a host-side change, sketched as a packet in *Residual
work* below.

## What building the adapter changed, and what it revealed

Writing a real adapter is how a contract stops being a guess. Two sets of
findings came out of it.

### Contract corrections (applied here)

The crate has never been published and has no consumers, so these were applied
in place at contract 1.0 rather than as a version bump. After the first named
consumer, the same four would require a major.

1. **`ReleaseLeaseRequest` gained `credential` and `reason`.** `ptah_release_work`
   requires both. The v1 shape could not have released a lease at all. The
   credential is `#[serde(skip)]`, so the wire shape is unchanged and the secret
   still cannot cross a JSON boundary; the claimant holds it, exactly as the
   runtime intends.
2. **`replayed` became `Option<bool>` on every receipt.** The control plane
   replays a stored idempotency receipt byte-for-byte, so a replay is
   *indistinguishable from fresh work on the wire*. Reporting `false` would
   have been a claim no adapter over that boundary can support. `None` means
   "the host does not report it"; the invariant a caller can still rely on is
   the one that matters — the same key never does the work twice.
3. **The battery's replay check now fails only on the dangerous outcome.** A
   second run under a used key, or a host claiming `Some(false)`, still fails.
   "Cannot tell" passes.
4. **The battery's fence check distinguishes three outcomes.** Fencing
   correctly passes; refusing to fence (`unsupported`) *skips*; silently
   accepting a stale fence still fails.

The battery also gained two lease checks, so the declared `control.lease`
capability is now exercised on any adapter that has a claimable work item.

### Bridge-side findings (not changed here)

Each of these is a real gap on the runtime side. None is fixed in this branch:
the bridge is outside this change's file allowlist, and three of the five sit
on or beside an active line.

1. **`ptah_submit_task`'s advertised `bounds` schema omits `maxTotalTokens`.**
   `merge_bounds` accepts it and `MCP_CONTROL_COORDINATOR.md` documents it, but
   `tool_input_schema` lists only `maxPromptBytes`, `maxRounds` and
   `maxDurationMs`, and every schema there is `additionalProperties: false`. A
   consumer that validates against the advertised schema — as the in-tree
   `McpControlClient` does — cannot use the documented token ceiling. The
   adapter sends the key only when the caller sets it, so a validating
   transport rejects it loudly rather than the adapter dropping a ceiling
   silently.
2. **The durable Run record carries no monotonic revision.** `updatedAt` in
   epoch milliseconds is the only monotonic non-decreasing quantity available,
   so that is what the adapter derives `Revision` from. Two commits inside one
   millisecond collapse to a single value, which a `RevisionWatermark` then
   treats as stale — conservative, but it can drop a real update.
3. **`ptah_steer` has no compare-and-set fence.** The queue mutators take
   `expected_version` / `expected_revision`; steering does not. The adapter
   refuses a fenced follow-up with `unsupported` rather than dropping the fence,
   because a fence that silently does not fence is worse than no fence.
4. **`ptah_create_session` accepts no `request_id`.** Session creation is not
   idempotent at the host, so a retry after a timeout can create a second
   session. `CreateSessionRequest::request_id` is not transmitted.
5. **The control plane exposes no host version.** `ptah_get_capacity` reports
   health and limits but no build identity, so `ServiceHostInfo` is supplied by
   the embedder from whatever it used to connect.

One contract-side observation, recorded as P2 rather than fixed:
`ArtifactDescriptor` requires `byteLen` and `digest`, which a listing cannot
know without materializing the body. The service adapter therefore leaves
`RunView.artifacts` empty and serves its one artifact under a stable id, and
the battery's three artifact checks skip against it.

### Capability discovery

`CapabilityDocument` names the asserting host (`kind`, `product`,
`host_version`), the moment it was asserted, the boundary limits a consumer must
respect, and one descriptor per capability. Availability is three-valued:

* `Available`
* `Unsupported { reason }` → `SdkErrorCode::Unsupported`, meaning *this host
  cannot*, e.g. a service build with no Computer Use ledger
* `Forbidden { reason }` → `SdkErrorCode::ForbiddenScope`, meaning *this
  credential may not*

A capability the host never mentioned is a fourth, distinct answer:
`SdkErrorCode::CapabilityUnavailable`. "I don't know about this" and "I refuse
this" are different facts and a consumer may reasonably act on each.

Two identifiers are **permanently forbidden** and stamped by
`CapabilityDocument::new` itself, overwriting whatever an adapter passed:

| Identifier | Why |
|---|---|
| `computer.control` | The runtime exposes no Computer Use mutation, grant issuance, evidence byte, or screenshot over its control plane, and a release gate snapshots that surface. This seam must not become the first hole in it. |
| `provider.credentials` | Provider authority is never delegated. Keys, routes, gateway config, and auth material have no representation on this boundary. |

Removing either is a contract **major** change and a security decision.

## Error taxonomy

`SdkErrorCode` is a mirror plus a small seam-local set. `origin()` says which.

| Code | Origin | Mirrors |
|---|---|---|
| `unauthenticated`, `forbidden_scope`, `workspace_mismatch`, `session_busy`, `capacity_exhausted`, `stale_version`, `cursor_expired`, `timeout`, `invalid_request`, `unsupported`, `conflict`, `internal` | Runtime | `orchestration::OrchErrorCode`, byte-identical wire tokens |
| `stale_observation`, `uncertain_outcome` | Runtime | `computer_use::ComputerErrorCode` |
| `transport_unavailable` | Seam | the adapter could not reach the host, or a stream dropped |
| `contract_version_unsupported` | Seam | contract major mismatch |
| `capability_unavailable` | Seam | the host never advertised this capability |
| `integrity_mismatch` | Seam | an artifact failed its declared size or digest check |

Unknown wire codes decode to `SdkErrorCode::Unknown(String)` and are
conservatively non-retryable, so a newer host can add a code without breaking an
older consumer.

`retry_disposition()` is three-valued on purpose:

* `Safe` — retry with the **same** `RequestId`; the host replays if the original
  attempt landed.
* `Never` — retrying cannot help.
* `Unsafe` — **`uncertain_outcome` only.** The mutation may or may not have
  applied. Collapsing this into "do not retry" would lose the one case that
  matters: an automatic retry here can double-apply real work.

## Idempotency, revisions, and cursors

* Every mutation carries a `RequestId`. Same key + same payload replays the
  original receipt with `replayed: true`; same key + different payload is
  `conflict`. This mirrors the runtime's durable idempotency receipts.
* Every mutation receipt reports the `Revision` it produced. Chain from the
  receipt rather than re-reading — a re-read can observe someone else's newer
  mutation and land you back where you started.
* `expected_revision` on a mutation is a compare-and-set fence; a stale fence is
  `stale_version` and leaves state untouched.
* `RevisionWatermark` implements the "strictly newer wins" rule for applying
  snapshots. The runtime publishes events *after* releasing its mutation lock,
  so publish order (`seq`) and commit order (`revision`) can differ; without the
  watermark a late-delivered older snapshot silently regresses a consumer's
  view. A non-advancing snapshot is `stale_observation`, not a silent no-op.
* `Cursor` is opaque. A consumer stores and echoes it, never does arithmetic on
  it, so a host may change its cursor encoding without a contract break. A
  cursor below the retained window is `cursor_expired` **carrying the retained
  range**, so recovery needs no second round trip.

## Public projection vs. operator data vs. authority-owned secrets

Three tiers, separated by **type**, not by transport-layer filtering. The
runtime's own Computer Use projection sets this precedent: "Anything a
coordinator is not allowed to observe is absent from the type itself rather than
filtered at the transport boundary."

### Tier 1 — public projection (this crate)

Run identity, lifecycle, stop cause, revision, execution mode, queue position,
applied bounds, usage accounting with its `complete` trust flag, progress,
workspace-relative changed files, artifact descriptors, evidence-backed
verification, and bounded non-transcript events.

### Tier 2 — operator/admin (absent from this crate)

Permission prompts and decisions, credential and keychain management, provider
profile editing, gateway configuration, Computer Use grant issuance, run
approval and promotion of reviewed code, host configuration, MCP server trust.
These stay on the local operator surface. ADR-002 §5 notes that today every
configured service bearer is operator-equivalent for `ptah_approve_run` and
`ptah_promote_run`; the SDK does not carry those operations, so adding one later
is an explicit decision rather than an accident.

### Tier 3 — authority-owned secrets (never cross)

Provider keys, bearer tokens, OS keychain material, Computer Use evidence
assets. The single secret that exists on this boundary is a work lease token: it
lives in `dto::LeaseCredential`, which is not `Serialize`, redacts its own
`Debug`, and is `#[serde(skip)]` inside `ControlLease`. A contract test asserts
the secret appears in neither the debug output nor the JSON.

### Specifically absent, and why

| Absent | Why |
|---|---|
| Prompt text, prompt previews, model prose, final responses | Transcript. The seam cannot tell "my own run" from "another client's run", so it carries the host's *evidence* (`VerificationView`) instead of the model's prose. |
| `AgentMessageChunk`, `AgentThoughtChunk`, `ShellOutput`, `FileEdit.unified_diff` | Transcript, and unbounded. |
| Absolute workspace paths, `GROKPTAH_HOME`, store paths | Internal storage. `WorkspaceRef` is an adapter-issued opaque handle; the adapter alone maps it to the canonical path the runtime authorizes against. |
| Provider names, routes, dialects, credentials | Provider authority. |
| Computer Use element labels, values, geometry, evidence tokens | Already denied at the runtime; denied here too. |

Two structural defenses back this up:

* `RelativePath` is the only path-shaped type on the boundary. It rejects
  absolute paths, `..` and `~` segments, drive letters, and UNC/verbatim
  prefixes — **on decode as well as on construction**, so a hostile or buggy
  host cannot smuggle one through JSON.
* `Label` and `BoundedText` strip control characters and bound length, so a host
  cannot inject terminal escapes or newlines into a consumer's UI or logs.

## Lifecycle

`dto::RunLifecycle` mirrors the runtime's `RunState` **exactly**: `queued`,
`running`, `completed`, `failed`, `cancelled`, `interrupted`, `limit_reached`.
`dto::StopCause` mirrors `RunStopCause`. `FollowUpDisposition` mirrors
`SteeringDisposition`. `ToolKind`/`ToolStatus` mirror `ToolCallKind`/
`ToolCallStatus`. A unit test pins the seven lifecycle wire tokens in the
runtime's declaration order.

There is no second state machine. If the runtime gains a state, this is a
contract major change, not an SDK-side translation.

## Versioning

`ContractVersion` is major.minor and describes **this crate's boundary** — not
the MCP transport `protocolVersion` and not any crate version.

* **Minor** bumps are additive only: new optional fields, new capability
  identifiers, new error codes. Older consumers keep working because unknown
  members decode into `SdkErrorCode::Unknown`, `CapabilityId::Unknown`, and
  `PublicEventKind::Unrecognized` rather than failing to parse.
* **Major** bumps remove or reshape something. A major mismatch is refused in
  *both* directions: an older host cannot satisfy a newer major, and a newer
  host has reshaped fields an older consumer would silently misread.

`negotiate()` returns the effective minor, `min(consumer, host)`, and a
`degraded` flag when the consumer is ahead. `Connected::require` additionally
refuses a capability whose `since` minor is above the negotiated effective
minor, even when the host says it is available — the consumer was not compiled
against that shape.

Compatibility rules for future changes:

| Change | Allowed in a minor? |
|---|---|
| Add an optional DTO field | Yes |
| Add a capability identifier | Yes |
| Add an error code | Yes |
| Add a `PublicEventKind` variant | Yes (older consumers see `Unrecognized`) |
| Add a trait method with a default body | Yes |
| Add a required DTO field | No |
| Rename or retype a field | No |
| Remove or repurpose a wire token | No |
| Change a lifecycle state | No |
| Make a permanently forbidden capability available | No |

**1.0 → 1.1** is the first exercise of that table, and every change in it is on
the additive side: a new capability identifier (`receipt.read`), new DTOs
(`ReceiptView`, `ReceiptStatus`, `OperationClass`), a new trait
(`RunObservatory`), and one new `AgentControlPlane` method with a default body
that fails closed. An adapter written against 1.0 still compiles and reports
the capability as absent — which is why the default body returns
`capability_unavailable` rather than an empty page.

## Conformance battery

`conformance::run_battery` drives any `Harness` through 24 checks: discovery,
forbidden-capability denial, submit, replay, key-reuse conflict, projection
readability, revision monotonicity, cross-session/cross-workspace/cross-tenant
denial, lost connection, uncertain send, follow-up acceptance and stale fencing,
event paging and resume, oversized page limits, cursor expiry, artifact
verification, artifact ceilings, digest mismatch, idempotent cancellation, and
a control-lease round trip.

A check whose precondition the harness cannot produce is reported as
**skipped**, never silently passed. A matrix that quietly counts unrunnable
checks as green is worse than no matrix.

It now runs against two adapters:

| Adapter | Result | Skips |
|---|---|---|
| `FakeControlPlane` | 24 passed, 0 failed, 0 skipped | none — the fake can produce every fault, which is what proves the checks run |
| `ServiceControlPlane` over a scripted `ptah_*` transport | 17 passed, 0 failed, 7 skipped | cross-tenant (one owner per host), uncertain send (no such wire state), retained-range on expiry (run events carry no `eventRange`), follow-up fencing (no CAS on `ptah_steer`), and three artifact checks (see the `ArtifactDescriptor` note above) |

A further test asserts the two adapters **agree on every check both can run**,
so a difference has to show up as an explicit skip rather than as drift. That
is the parity property the matrix exists for; what it does not yet cover is a
*live* host, which is what ADR-002 §7 step 3 actually asks for.

## How ContextDesk consumes this

1. **Depend on the crate, not the runtime.**
   ```toml
   grokptah-agent-sdk = { path = "../GrokPtah/crates/codegen/grokptah-agent-sdk" }
   ```
   Turn off `default-features` and enable only `fake` if you want the
   deterministic adapter without the battery, or neither for a production-only
   dependency graph.

2. **Build the whole UI against `FakeControlPlane` first.** It produces every
   failure mode the boundary defines — dropped connections, uncertain sends,
   expired cursors, corrupted artifacts, cross-tenant denial — with no GrokPtah
   process running and no provider calls.

3. **Program against `AgentControlPlane`, never a concrete adapter.** Hold a
   `Arc<dyn AgentControlPlane>` in app state. Swapping fake → service → desktop
   is a construction-site change.

4. **Connect once, then gate on capabilities.**
   ```rust
   let connected = plane.connect().await?;
   let can_lease = connected.require(&CapabilityId::ControlLease).is_ok();
   ```
   Render a disabled control with the denial reason rather than discovering the
   refusal on click. `Availability::Unsupported` and `Forbidden` carry a reason
   string that is safe to show.

5. **Keep a `RevisionWatermark` per resource** and feed every snapshot through
   it before applying. Chain fenced mutations from the previous receipt's
   revision.

6. **Route every mutation failure through `recover_mutation`.** Never write your
   own retry policy: `ReconcileFirst` exists so an uncertain mutation cannot be
   auto-retried into a double effect.

7. **Run the battery in your own CI** once you have a real adapter. That is the
   evidence ADR-002 §7 step 4 asks for, and it is what turns ContextDesk from a
   prospective consumer into a named one.

## Residual work

### P0 — none

The shipped slice is self-consistent: contract, fake, battery, and tests all
pass, and no existing runtime behavior changed.

### P1 — required before a real consumer ships against a live host

1. ~~**Service adapter** over `grokptah-service`~~ — **delivered**. See *The
   service adapter* above. What it still needs is a real transport: an embedder
   must supply `McpTransport` over its own HTTP client, and no such transport
   ships here.
2. **A live two-host run of the battery.** The matrix runs against the fake and
   against the service adapter over a *scripted* transport. Neither is a live
   host. Standing this up means booting `grokptah-service` against a disposable
   `GROKPTAH_HOME`, pointing a real transport at it, and running the same
   battery — plus the same again against the desktop's embedded control server.
   Until then, "parity" covers wire shape, not runtime behavior.
3. **Desktop adapter** over the in-process `OrchestrationService`, for hosts
   that embed the runtime rather than talk to it.
4. **Run the battery in CI** once (2) exists, with stated pass criteria and an
   allowed-skip list per host.
5. **`run.events.live`**: add a `subscribe_events` method returning a bounded
   stream, mapping the SSE channel plus `notifications/ptah_recovery`. A gap
   must surface as `transport_unavailable` with the resume cursor, never as a
   silently short stream.
6. **`WorkspaceRef` issuance**: define how a host mints stable refs across
   restarts. A ref that changes on restart breaks a consumer's saved state; a
   ref derived from a guessable path is confirmable against it. A per-home
   stable random id persisted alongside the allowlist entry is the expected
   shape, and it closes the residual weakness in the adapter's keyed digest.
7. **The five bridge-side findings above**, each of which needs an owner on the
   runtime side.
8. **A host-side receipt read**, to make `receipt.read` real over the service
   boundary. Implementation packet, for whoever owns the control plane:

   * **Tool**: `ptah_list_receipts`, a read. Required arguments
     `session_id`, `workspace`, `run_id` — the same scope triple every other
     run read takes, so it inherits the existing allowlist-then-scope gate and
     the identical `forbidden_scope` denial. Optional `after` (opaque) and
     `limit` (1–500), matching `ptah_get_events`.
   * **Source**: `OrchStore`'s durable idempotency receipts, filtered to
     `receipt.run_id == run_id`. Receipts with no run are not listed.
   * **Response**: `{ receipts: [...], nextCursor }`, each receipt
     `{ requestId, tool, status, errorCode, payloadHash, runId, createdAt }`.
     Note what is *not* in it: `response` and the error `message`. Both are the
     reason this cannot simply serialize `IdempotencyReceipt` — the stored
     response replays a mutation's full body, and error messages embed absolute
     paths (`orchestration/authz.rs` formats one into a `workspace_mismatch`).
     The projection must be built, not derived by `serde`.
   * **Retention**: already bounded — the store keeps the newest 1,000
     completed/failed receipts and expires them after 7 days. Say so in the
     tool's contract, because a consumer must not read absence as "never
     happened".
   * **Adapter side**: map it in `service.rs` alongside the other reads, flip
     `receipt.read` from `Unsupported` to derived-from-`tools/list`, and the
     battery's `receipts.are_scoped_and_do_not_echo_the_request` check stops
     skipping. No SDK type changes — the contract for this already exists and
     is exercised against the fake.

### P2 — worth doing, not blocking

6. **Authorship-scoped transcript projection.** A consumer that submitted a run
   arguably may read its own final response. That needs a per-run authorship
   check (`clientId` on the durable record) plus a new capability
   (`run.transcript.read`) and a bounded projection. Do not add this by widening
   `RunView`; add it as a separate, separately-denied capability.
7. **Computer Use read projection** behind `computer.read`, mapping the four
   `ptah_*_computer_*` tools. The projection is already redaction-safe; the work
   is DTO mirroring plus the `ComputerErrorCode` subset. Mutation stays
   permanently forbidden.
8. **Workload/routine surface** (`ptah_list_work`, `ptah_create_routine`, the
   manager-plan tools). Large, and only worth carrying once a consumer needs it.
9. **TypeScript DTO generation** from the Rust types, so a web or desktop
   frontend consumes the same contract without a hand-maintained mirror.
10. **Split `ArtifactDescriptor` listing from verification.** Requiring
    `byteLen` and `digest` on a descriptor forces an adapter to materialize a
    body just to list it. Making both optional until fetch would let the service
    adapter advertise its artifacts.
11. **Publication decision.** `publish = false` until ADR-002 §7 step 5 is met:
    a named compatibility and version owner maintaining the matrix for a real
    external consumer.

## Explicitly out of scope

Provider-authority internals (profiles, credentials, dialects, qualification)
and packaged VM / Computer Use qualification are disjoint from this seam and are
not touched by it.
