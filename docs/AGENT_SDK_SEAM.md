# GrokPtah agent SDK seam

Status: **contract shipped, adapters pending.** This document is the design
packet and the implementation guide for `crates/codegen/grokptah-agent-sdk`.

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

| Method | Capability | Backing runtime surface |
|---|---|---|
| `capabilities` | — | *new*: assembled by the adapter from host config; see below |
| `create_session` | `session.create` | `ptah_create_session` |
| `list_sessions` | `session.list` | `ptah_list_sessions` |
| `submit_task` | `task.submit` | `ptah_submit_task` |
| `observe_run` | `run.observe` | `ptah_get_run` + `ptah_get_progress` + `ptah_get_handoff` + `ptah_get_changes` |
| `stream_events` | `run.events.page` | `ptah_get_events` |
| `request_follow_up` | `run.followup` | `ptah_steer` |
| `cancel_run` | `run.cancel` | `ptah_cancel` |
| `acquire_control` / `release_control` | `control.lease` | `ptah_claim_work` / `ptah_release_work` |
| `fetch_artifact` | `artifact.fetch` | `ptah_review_run` (diff), `ptah_get_test_results` (report) |

`run.events.live` (the scoped SSE channel and its `ptah_recovery` gap notice) is
declared but not yet in the trait — see P1.

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

## Conformance battery

`conformance::run_battery` drives any `Harness` through 22 checks: discovery,
forbidden-capability denial, submit, replay, key-reuse conflict, projection
readability, revision monotonicity, cross-session/cross-workspace/cross-tenant
denial, lost connection, uncertain send, follow-up acceptance and stale fencing,
event paging and resume, oversized page limits, cursor expiry, artifact
verification, artifact ceilings, digest mismatch, and idempotent cancellation.

A check whose precondition the harness cannot produce is reported as
**skipped**, never silently passed. A matrix that quietly counts unrunnable
checks as green is worse than no matrix. Against the fake, zero checks skip —
that is what proves the checks actually run.

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

1. **Service adapter** (`McpControlPlane`) over `grokptah-service`, mapping the
   table above. It is the only component that may hold canonical workspace
   paths, and it owns the `WorkspaceRef` ↔ path map. Error mapping is a string
   match on `error.data.code`, since the mirror tokens are byte-identical.
2. **Desktop adapter** over the in-process `OrchestrationService`, so the same
   battery runs on both hosts. This is ADR-002 §7 step 3's remaining half.
3. **Run the battery against both hosts in CI**, with stated pass criteria and
   an allowed-skip list per host. Until this exists, "parity" is an assertion.
4. **`run.events.live`**: add a `subscribe_events` method returning a bounded
   stream, mapping the SSE channel plus `notifications/ptah_recovery`. A gap
   must surface as `transport_unavailable` with the resume cursor, never as a
   silently short stream.
5. **`WorkspaceRef` issuance**: define how a host mints stable refs across
   restarts. A ref that changes on restart breaks a consumer's saved state; a
   ref derived from the path leaks the path. A per-home stable random id
   persisted alongside the allowlist entry is the expected shape.

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
10. **Publication decision.** `publish = false` until ADR-002 §7 step 5 is met:
    a named compatibility and version owner maintaining the matrix for a real
    external consumer.

## Explicitly out of scope

Provider-authority internals (profiles, credentials, dialects, qualification)
and packaged VM / Computer Use qualification are disjoint from this seam and are
not touched by it.
