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
PR body. All five agree with their PR-reported heads.

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

The G1–G4 host authority/effect train owns, and this train therefore **depends
on but does not implement**:

| Authority | Issues / PRs | Owner |
| --- | --- | --- |
| Host lifecycle, process-lock ownership, ordered shutdown, durable-write sealing | #455 / #468 | G1 |
| Canonical principal, auth generation, delegation | #477 / #460 | G2 |
| Capability / effect generation, queue-ownership binding | #458 / #461 | G3 |
| Audit v2 generations, intent/effect/outcome ledger | #462 / #469 | G4 |

This train binds those through **typed provisional seams** that record what is
*not* yet authoritative, so that when G1–G4 lands only the mint path changes and
no consumer contract breaks.

## 4. Disposition

| Donor | Component | Disposition | Reason |
| --- | --- | --- | --- |
| #467 | `RunStopDetail` — structured stop reason next to `RunStopCause` | **KEEP (semantics)** | Correct: one terminal authority, a qualifier beside it, `#[serde(default)]` so old records load. Re-expressed here as `TerminalObservation` + `StopDetail` without adding a second state machine. |
| #467 | SHA-256 content digest of the observation | **KEEP (algorithm)** | The published head hashes real content bytes with a domain separator. Sound, and stable across toolchains unlike `DefaultHasher`. |
| #467 | Digest **call site** — `round_observation_digest(&messages)` | **REWRITE — defect** | `messages` holds tool content that `host.rs` already truncated to 24,000 bytes (`host.rs:8865`, `host.rs:8978`). Two rounds whose raw outputs differ only after byte 24,000 hash identically, are classified `inert_repeat`, and stop the run at the 4-round inert ceiling while it is genuinely progressing. The digest must be taken from the **raw** output before any bounded projection. |
| #467 | Content-free shape digest (length / line count / digit histogram) | **REJECT** | Superseded on the donor's own head. It carried an acknowledged collision residual that a raw-content digest removes outright. |
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

A provider-neutral, transport-free core in
`crates/codegen/grokptah-agent-bridge/src/durable/`, plus the minimum wiring
that closes the one live defect above.

1. **Typed terminal observations and retry decisions** — `TerminalObservation`
   and `RetryDecision` are derived from evidence, never guessed.
2. **Raw-output digests before bounded projections** — a `BoundedProjection`
   can only be constructed *from* a `RawObservation` that has already been
   digested, so the ordering is enforced by the type system rather than by
   convention.
3. **No false no-op / stationarity** — a repeat is inert only when the call
   signature *and* the raw observation digest are both unchanged.
4. **Durable work claims and revisions** — revision-CAS, stale-revision
   refusal, duplicate-worker refusal, idempotent re-claim.
5. **One provider-send lattice** — #494's semantics, with no second ledger.
6. **Cancellation that proves the actual turn idle** — a cancel reports
   `Cancelled` only once every registered lease is observed idle.
7. **Registered-before-start effect supervision** — an effect that never
   registered cannot start, so a crash always leaves a record to recover.
8. **Crash/restart recovery** with malformed and truncated records counted and
   surfaced instead of silently skipped.
9. **Bounded retries, resources, and event/audit growth.**
10. **A provider-neutral embeddable manager/SDK boundary** exposing no raw
    transport and refusing a self-asserted operator escape.

## 6. Nonclaims

- Not qualified, not certified, not release-gated.
- No human review has occurred.
- No soak run. No live provider call, no credential read, no live small-model
  sampling. Every fixture is synthetic and offline.
- No self-hosting qualification is claimed or implied.
- Does not close #301, #455, #461, #478 or #492, and does not supersede,
  invalidate, or propose closing any donor PR.
- A green hosted run would show only that the repository's own gates pass.
