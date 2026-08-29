# Durable agent / self-hosting consolidation train

Canonical train #2 of the #492 release-train plan: *"Durable agent, SDK,
external-worker, and embeddable manager train"*. It is deliberately **not** the
G1–G4 host authority/effect train, and it does not reimplement any part of it.

Base of record: `67e29bd34dc64049432c715c93c2cef2185c63ea` (`origin/main`).

## 1. Exact donor ancestry

Every donor is a *direct* branch whose merge-base with `main` is `main` itself.
There is no stacking anywhere in this set, so donor semantics can be compared
pairwise without replaying intermediate lineages.

| PR | Branch | Head | Merge-base with `67e29bd` | Commits ahead | Files | Diff |
| --- | --- | --- | --- | --- | --- | --- |
| #467 | `claude/stationarity-stop-detail-v1` | `b75100c` | `67e29bd` (= main) | 6 | 21 | +3013 / −34 |
| #468 | `claude/host-runtime-shutdown-v1` | `17e0760` | `67e29bd` (= main) | 8 | 46 | +6361 / −598 |
| #470 | `claude/durable-work-graph-main-v1` | `a847004` | `67e29bd` (= main) | 6 | 8 | +4838 / −102 |
| #471 | `claude/agent-sdk-live-adapters-v1` | `2a952b4` | `67e29bd` (= main) | 31 | 49 | +18628 / −195 |
| #494 | `claude/grokptah-478-provider-attempt-mdmvq4` | `6b871d7` | `67e29bd` (= main) | 2 | 22 | +7927 / −685 |

Every head above was verified against the branch as published, not against the
PR body. All five agreed with their PR-reported heads at the time of analysis.

**Observed drift.** `claude/host-runtime-shutdown-v1` (#468) advanced from
`17e0760` to `459358d` while this train was being built. Nothing here is
derived from that branch — its disposition is *not this train's* (see §4) — so
the move does not invalidate any decision below. It is recorded because a
consolidation map that silently goes stale is worse than one that says where it
was taken from. The other four heads were unchanged, and `origin/main` was
still `67e29bd34dc64049432c715c93c2cef2185c63ea`, when this train was pushed.

## 2. Exact changed-file overlap

Files touched by more than one donor. These are the collision surface: merging
the donors as published would require reconstructing each of these files by
hand.

| Donors | File |
| --- | --- |
| #467 #468 #470 #471 | `crates/codegen/grokptah-agent-bridge/src/orchestration/store.rs` |
| #467 #468 #470 #471 | `crates/codegen/grokptah-agent-bridge/src/orchestration/service.rs` |
| #467 #468 #471 #494 | `crates/codegen/grokptah-agent-bridge/src/host.rs` |
| #467 #468 #471 | `crates/codegen/grokptah-agent-bridge/tests/orchestration_control.rs` |
| #467 #470 #471 | `crates/codegen/grokptah-agent-bridge/src/orchestration/mod.rs` |
| #467 #468 #494 | `crates/codegen/grokptah-agent-bridge/src/lib.rs` |
| #467 #468 #494 | `crates/codegen/grokptah-agent-bridge/src/host_helpers.rs` |
| #468 #471 | `desktop/src-tauri/src/commands.rs` |
| #468 #471 | `crates/codegen/grokptah-agent-bridge/tests/workload_mcp.rs` |
| #468 #471 | `crates/codegen/grokptah-agent-bridge/tests/orchestration_adversarial.rs` |
| #468 #494 | `crates/codegen/grokptah-agent-bridge/src/provider_qualification.rs` |
| #467 #471 | `crates/codegen/grokptah-agent-bridge/src/orchestration/types.rs` |
| #467 #468 | `tests/{mcp_continuity_probe,mcp_coordinator_campaign,mcp_soak_hardening,mcp_streamable_transport,native_executor_mcp}.rs` |
| #468 #471 | `crates/codegen/grokptah-agent-bridge/src/mcp_control.rs` |

`host.rs` (12,524 lines on main), `orchestration/store.rs` (6,377) and
`orchestration/service.rs` (7,910) are each rewritten by four donors. That is
the structural reason this train does not attempt a donor merge.

## 3. Where the G1–G4 boundary falls

PR #497 (`claude/grokptah-authority-consolidation-ddsn7x`, head `10f9fab`, base
`67e29bd`) is the canonical G1–G4 host authority spine. It owns, and this train
therefore **does not implement in any form**:

| Gate | Authority | Owner |
| --- | --- | --- |
| G1 | Host-issued principal root, credential incarnations, auth generations | #497 |
| G2 | Sealed capabilities and one-use leases, `ActorClass` | #497 |
| G3 | The physical-send attempt lattice and `PhysicalSendPermit` | #497 |
| G4 | Typed, hash-chained audit | #497 |

An earlier revision of this train shipped its own `durable::send` lattice and a
`durable::sdk` operator grant. Both were **withdrawn** after an exact-head
audit: they were a second public send authority beside G3, and
`grant_operator_for_host(GrantProvenance::Canonical)` was a public
self-elevation path — the same defect #497 records as its own defect 5
(self-asserted operator authority) and defect 4 (serde-minted authority). A
second copy of an authority is exactly what #478 and #492 exist to prevent, and
"provisional" markers do not make one safe. `durable::claim`, `durable::effects`,
`durable::cancel` and `durable::journal` were withdrawn with them: their
semantics are only safe once wired onto #497, #468 and #471, and shipping them
unwired invited exactly that mistake.

## 4. Disposition

| Donor | Component | Disposition | Reason |
| --- | --- | --- | --- |
| #467 | `RunStopDetail` — structured stop reason next to `RunStopCause` | **KEEP (semantics)** | Correct: one terminal authority, a qualifier beside it, `#[serde(default)]` so old records load. Re-expressed here as `TerminalObservation` + `StopDetail` without adding a second state machine. |
| #467 | SHA-256 content digest of the observation | **KEEP (algorithm)** | The published head hashes real content bytes with a domain separator. Sound, and stable across toolchains unlike `DefaultHasher`. |
| #467 | Digest **call site** — `round_observation_digest(&messages)` | **REWRITE — defect** | `messages` holds tool content that `host.rs` already truncated to 24,000 bytes (`host.rs:8865`, `host.rs:8978`). Two rounds whose raw outputs differ only after byte 24,000 hash identically, are classified `inert_repeat`, and stop the run at the 4-round inert ceiling while it is genuinely progressing. The digest must be taken from the **raw** output before any bounded projection. |
| #467 | Content-free shape digest (length / line count / digit histogram) | **REJECT** | Superseded on the donor's own head, which documents why: the vector collided, so `"phase: build"` and `"phase: test"` were indistinguishable and an advancing status line read as frozen. |
| #467 | `ObservationDigest` opacity — no `Serialize`, no `Display`, no byte accessor, `Debug` redacted | **KEEP (adopted)** | Correct, and better than this train's first revision, which derived `Serialize` over a full SHA-256 and then claimed in its own PR body that the digest was never published. Adopted verbatim in intent. |
| #467 | `ActiveTaskWaitWitness`, `ActiveWaitState`, `round_is_witnessed_wait` | **KEEP (rewritten)** | The right shape for the wait exemption: host-issued, outstanding-only, session-bound, generation-bound, deadline-bounded, and exempting the inert ceiling only. **This train's first revision missed it entirely** — the donor was surveyed for its digest and not for its stop/wait evidence — so a legitimate long wait would have stopped at the inert ceiling. Recorded as an error in this map's first pass. |
| #467 | PR-body claim that the digest is "content-free features only" | **STALE** | Does not describe head `b75100c`, which hashes full content bytes. Recorded so the next reviewer does not trust the body over the code. |
| #468 | Ordered shutdown, `HostRuntime` non-`Clone`, `DurableWriteGuard`, `WriteLease`, canonical `owner_key` | **REJECT for this train — G1** | This is the G1 authority itself. Duplicating it here would create the second lifecycle authority #492 exists to prevent. |
| #468 | `register_shutdown_hook` seam; run stopped by shutdown finalizes `Interrupted`; bounded finalize retry | **KEEP (dependency)** | This train's effect supervision registers *before* start so a G1 shutdown join has something to join, and its retry budgets are bounded by construction. |
| #470 | Durable work graph: cycle rejection, admission reasons, provenance fail-closed | **KEEP (semantics)** | Correct and already scoped to the existing Work ledger. Not re-implemented; this train's claim ledger is the revision/ownership half only. |
| #470 | Review quorum, `VerifiedPrincipal`, `AuthContext` verdicts | **REJECT for this train — G2** | The donor itself keeps this crate-internal and off every transport pending #460. Correct call; it stays G2's. |
| #470 | Managed precheck → intent → claim non-atomicity | **KEEP (constraint)** | This train's claim ledger makes the claim the authority and revision-CAS the arbiter, matching the donor's stated rule. |
| #471 | Versioned SDK conformance battery run against real hosts | **KEEP (direction)** | The right shape for a second consumer. Not vendored: 18,628 lines and 49 files, most of it a duplicate SDK package. |
| #471 | Principal binding on reads, idempotency scope, HMAC cursors | **REJECT for this train — G2/G3** | The donor marks its own scope derivation "provisional, not canonical authority" and defers to #460/#458. |
| #471 | "Bearer plus operator authority remains operator-equivalent" | **KEEP (as the defect to close)** | This train's manager/SDK boundary refuses a self-asserted operator escape: operator authority cannot be constructed by an embedder. |
| #494 | Send lattice states, transitions, delivery knowledge, retry rule | **KEEP (semantics, carried)** | The design is right: `Preparing` fsynced before admission, `Sending` before the send future, only transport evidence advances, `Uncertain` non-terminal and never auto-retried. Carried here as a provider-neutral, transport-free core. |
| #494 | Transport-error classification (`is_connect() && !is_timeout()` ⇒ `NotSent`) | **KEEP (rule)** | Preserves the uncertain / not-sent / settled distinction correctly. Re-expressed over an abstract evidence type so it is testable without `reqwest`. |
| #494 | Durable fsync ledger, hard-link ordinal allocation, crash-cut harness, 8-call-site rewiring | **REWRITE above G1–G4** | It binds #477 principal, #458 capability, #455/#468 lifecycle, #461 queue-ownership and #462 audit generations as `Provisional`. Landing the durable ledger before those authorities exist would freeze provisional identities into durable records. |
| #494 | Deleting `read_bounded_response_body` / `classify_transport`; one completions-URL definition | **KEEP (direction)** | Removing superseded read paths rather than leaving them reusable is correct, and belongs with the call-site rewiring above G1–G4. |
| #494 | Unbounded attempt-record retention (donor residual) | **REJECT** | This train bounds event/audit growth by construction rather than shipping an unbounded ledger. |

## 5. What this train implements

Held in `crates/codegen/grokptah-agent-bridge/src/durable/`, plus the host
wiring. **No authority of its own**: nothing here mints, seals, or grants.

### Public — the stationarity decision

1. **Raw-output digests before bounded projections.** A `BoundedProjection` can
   only be built *from* a `RawObservation` that has already been digested, so
   the ordering is enforced by the type system. This is where this train
   improves on #467, whose digest is taken from the already-truncated
   transcript and so cannot see a change past 24,000 bytes.
2. **The digest is opaque.** No `Serialize`, `Deserialize`, `Display` or byte
   accessor; `Debug` redacted. Compared in-process and discarded. `StopDetail`
   carries no digest and no fingerprint.
3. **No false no-op / stationarity.** Inert requires the call signature *and*
   the raw observation to be unchanged. The unchanged suffix restarts at each
   change, so a run that advances once and then freezes still stops.
4. **A host-issued wait witness**, rewritten from #467: authorized id,
   outstanding-only state, owning session, a generation that changes when an id
   is recycled, and a bounded deadline. Lifts the *inert* ceiling only.

### Crate-internal — supervision of the host's own work

5. **Registered-before-start effect supervision.** Every tool dispatch is
   registered before it starts, so a turn always knows what is in flight.
   `EffectRegistry::register` is the only source of an `EffectTicket` and
   `start` requires one, so an unregistered start does not compile.
6. **Cancellation that proves the turn idle.** Flipping the cancellation token
   is the *request*. `turn_cancellation_settled` answers `Some(false)` while
   registered effects are still active — the state `main` reports as
   "cancelled" today — and `Some(true)` only once nothing is in flight. A
   cancel also seals admission, so a racing round cannot start new work behind
   a turn that is already stopping.

Both are `pub(crate)`, unreachable from outside the crate, so they cannot be
mistaken for or presented as authority.

### Evidence about the ledger that already exists

`tests/durable_work_adversarial.rs` drives the **real** `OrchStore` rather than
a model of it, because a second claim ledger would be a second authority:

- duplicate workers racing one item yield exactly one lease;
- a claim survives a restart with its revision, and still refuses a second
  claimant;
- a malformed work record makes the store **fail closed at open** rather than
  silently shrinking the ledger — including under repeated corruption;
- **characterization:** `save_work_item` has no revision compare-and-set (the
  store has one for manager plans, `save_manager_plan_with_work_cas`, but not
  for work items), so a stale generic save still clobbers a newer revision.
  #470 closes this; the test is written so a fix shows up as a deliberate
  change rather than as silence.

Ceilings, ordered by strength of evidence: true no-op 4 · nudge 8 · inert 10 ·
identical-call 16 · advancing has no stationarity ceiling.

### Already on `main` — tested, not reimplemented

Four goal items turned out to be satisfied by code that already exists. Building
a second copy of either would have been the same duplication mistake in a new
place, so this train tests them instead:

- **Bounded event/audit growth.** `event_bus.rs` already enforces
  `MAX_JOURNAL_BYTES` (16 MiB), a per-line cap, capacity-based trimming, and
  `OrchStore::prune_retention`. An earlier revision of this branch shipped its
  own bounded journal; it was withdrawn as redundant.
- **Durable work claims and revisions.** `OrchStore::claim_work` is already
  durable, lease-scoped and revision-bearing. The evidence above is what this
  train adds.
- **Bounded retries.** `WorkPolicy.retry.max_attempts` is validated (`> 0`,
  `<= 100`) and enforced at `workload.rs:503`, and `ManagedRetryCause` is a
  typed retry cause. An earlier revision of this branch shipped its own
  `RetryBudget`; withdrawn as redundant.
- **Run crash/restart recovery.** Reopening the store marks an in-flight run
  `RunState::Interrupted` with `RunStopCause::Interrupted`, already covered by
  the store's own tests. Effect-level recovery is the part that is missing, and
  it needs durable effect records (#497 G4).

### Uncertain / not-sent / settled

`main` retried a provider request on **any** transport error. The retry site
(`host_helpers.rs:2091`) even distinguished a timeout from a refused connection
to word the error message, then retried both identically, up to three times.

A timeout can happen after the request was fully written and the provider has
already done the work, so re-sending it duplicates a model invocation rather
than recovering a lost one. Only a connection that was never established proves
no byte moved. That is #478's acceptance criterion — *automatic retries stand
down for any attempt whose delivery is not proven NotSent* — and the retry site
is now gated on it.

`durable::delivery` holds the rule and nothing else: no state, no records, no
permits, no ordinals, nothing to persist. The durable attempt lattice that does
hold those is #497's G3, and this deliberately does not approximate it. A source
guard keeps the gate in place.

The remaining half of the distinction — *settled*, and durably recording which
attempt reached which state — is still #497's. `AttemptDisposition` on `main`
(`{Completed, HttpError, TransportError, Timeout, Cancelled, ProtocolError}`)
still classifies what went wrong without saying whether the request arrived; a
characterization test pins that.

The second retry site (`:2228`) is deliberately unchanged: it fires after the
provider *answered* with an HTTP status, so the request demonstrably arrived and
was refused rather than processed.

### Not here, and why

| Goal item | Where it belongs |
| --- | --- |
| One provider-send lattice; uncertain/not-sent/settled | #497 **G3**. A second lattice is what #478 forbids, and the first attempt here was forgeable. |
| Minting operator authority for an embedder | #497 **G1/G2**. What *is* asserted here is the property against the real surface: `CONTROL_TOOLS` exposes no transport, provider, credential or self-elevation tool, and stays disjoint from `FORBIDDEN_TOOLS`. |
| Durable effect crash/restart recovery | Needs durable effect records — #497 **G4**. The registry is per-turn and in-memory, so there is nothing to recover *from*; durable **work** restart recovery is covered above against the real store. |
| Uniform malformed-record accounting on the run and idempotency read paths | `store.rs`, the four-donor collision surface and #470's seam. Work records already fail closed; those two paths still skip in silence. |

## 5a. Exact-head audit, and what it changed

An independent exact-head audit at `4178822` returned **REWRITE / selective
stationarity donor**. Its findings were checked against this branch's own source
rather than accepted or argued with, and all six held:

| Finding | Verdict | Resolution |
| --- | --- | --- |
| `durable::send` is a second public send authority beside #497's G3 | correct | module withdrawn |
| The send lattice was internally forgeable — `observe` terminalized without a receipt, `settle` took caller-supplied audit state, `resolve_uncertain(granted: bool)` was caller assertion, permits were not scope-bound, recovery accepted forged ordinals | correct, all five | module withdrawn |
| Public `grant_operator_for_host(GrantProvenance::Canonical)` is self-elevation | correct | module withdrawn |
| A sticky advance flag made `A,B,B,B…` advance forever | correct | unchanged suffix now restarts at each change; regression test added |
| Host-witnessed active waits regressed against #467 and stopped at 10 | correct | #467's witness design rewritten in |
| The digest derived `Serialize` and was serialized by `TerminalObservation`, contradicting the privacy claim | correct | digest made opaque; `TerminalObservation` withdrawn; `StopDetail` carries no fingerprint |

The three withdrawn-module findings share one root cause worth recording: #497
did not exist when this train's donor map was written, so its send and operator
work was built as a "typed core awaiting G1–G4" rather than recognised as a
duplicate of an authority that had since landed. A consolidation map is only as
current as its last read of the open-PR set.

## 6. Nonclaims

- Not qualified, not certified, not release-gated.
- No human review has occurred.
- No soak run. No live provider call, no credential read, no live small-model
  sampling. Every fixture is synthetic and offline.
- No self-hosting qualification is claimed or implied.
- Does not close #301, #455, #461, #478 or #492, and does not supersede,
  invalidate, or propose closing any donor PR.
- A green hosted run would show only that the repository's own gates pass.
