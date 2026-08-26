# Semantic Help authority — handoff

This document is the controlling reference for the Semantic Help authority
lane. It records what the boundary is, why each piece is shaped the way it is,
what is proven and by which gate, and what is deliberately not done yet.

It did not exist when this lane's third round began. The round's task
description named it as controlling; it was not present in any ref, and the
work proceeded from the enumerated blockers instead. This file is that
omission repaired, written from what the code now does rather than from what
was intended.

## The boundary in one picture

```
renderer / published client          host                       corpus
─────────────────────────────        ────────────────────       ──────
searchHelpCorpus (offline)     │
verifyHelpClaimSpan            │
checkHelpClaimCoverage         │
validateHelpAnswerResponse     │
                               │
GrokPtahBrokerClient ──────────┼──▶ authorize_against_manifest ──▶ SourceManifest
  .authorizeHelp()             │      (grant verified here)
  .answerHelp()  ──────────────┼──▶ HelpAnswerExecutor
                               │      (admission verified here)
Tauri: help_authorize ─────────┤      one provider call, deadlined
       help_answer_execute ────┘      ExecutionReceipt (no artifacts)
```

Everything on the left runs in code the caller controls, and none of it
decides anything. Everything to the right of the line requires host key
material to construct.

## What each piece exists to prevent

### `grant.rs` — host-minted grants

A caller used to describe its own principal, capabilities, and project
membership, and the authority evaluated that description. The description was
the thing being checked, supplied by the party it constrained.

A `HelpGrant` is minted by the host from its own view of the principal and
MAC'd with `GrantMintingKey`. `mac_fields` sorts and length-counts the project
and capability lists, so a project list cannot be confused with a capability
list of the same contents.

*Proven by* `grant_tests.rs`: a twelve-mutation forgery table, a foreign-key
grant, a fabricated MAC, stale policy and revision, index mismatch, expiry at
both ends, action replay, and visibility-cap narrowing.

### `manifest.rs` — server-owned source records

The source digest covered an id, a path, a heading, and a visibility label —
the *metadata about* a section, not the section. Two documents sharing a
heading digested identically, so substituting the bytes behind a citation
changed nothing any check could see. `SourceDescriptor.digest` was parsed and
never compared to anything.

Now: `source_digest` covers the exact normalized section bytes plus the
metadata; `reject_duplicate_keys` scans raw JSON before `serde_json` can keep
the last of two identical keys; `describe()` rebuilds descriptors from the
manifest so a caller cannot relabel private → public; `enforce_descriptor()`
is the comparison the digest field never had.

*Proven by* `manifest_tests.rs` and `omission_tests.rs`.

### `admission.rs` — host-minted answer routes

`createHelpAnswerRoute` digested the caller's own fields. That digest is
self-consistent for any values the caller picks: it proved the fields were
unedited after the caller chose them, never that the host would allow them.

An `AnswerAdmission` binds the route, the grant and policy revisions, the
corpus/index/manifest digests, and **the digest of the exact request body it
admits**. The last binding is what stops replay: an admission obtained for a
harmless question cannot be reattached to another one.

*Proven by* `admission_tests.rs`, including a fifteen-mutation forgery table
and an explicit replay case.

### `claims.ts` — claim-bound coverage

Support was a ratio: total quoted code points against total answer length.
Nothing said which claim a citation was evidence for, so an answer could make
five claims, quote one passage supporting the first, and pass.

Coverage is now decided per claim, over a segmentation the *validator* owns —
a provider that chose its own segmentation could declare the whole answer one
claim. Every material claim needs a citation; every citation needs a material
claim; every citation must share vocabulary with the claim it names; spans may
not overlap in UTF-8.

**What this does not prove.** Vocabulary overlap is not entailment. It
separates "this quote is about this claim" from "this quote is about something
else in the same article". It does not show the quote supports what the claim
asserts, and it is not represented anywhere as doing so.

*Proven by* `helpClaims.test.ts` and the coverage cases in
`helpAnswer.test.ts`.

### `grokptah-help-answer` — supervised execution

- **Only the host can execute.** `HelpAnswerExecutor::new` requires
  `GrantMintingKey` material. A renderer cannot build an executor, so it cannot
  hand one its own provider.
- **Bounded and supervised.** Fixed pool, bounded queue, deadlines enforced by
  a supervisor thread rather than by whoever remembers to `join`.
- **Capacity held until quiescence.** A provider that ignores its cancellation
  token keeps its slot. `ExecutionOutcome::Abandoned` and `stats().stuck`
  report that honestly, because a "cancelled" answer that is still talking to a
  gateway is not cancelled.
- **Zero-artifact receipts.** Ids, digests, counts, timings. Never the
  question, the answer, a quote, a path, or a provider's own error string.

*Proven by* `executor_tests.rs`, including a deaf provider, a queue at its
bound, post-admission drift, and an admission that expired while queued.

### The published client

`@grokptah/client` used to re-export the whole Help barrel: a browser consumer
got `authorizeHelpDecision` and `createHelpExecutor` and could decide, in code
it controls, whether it was allowed to see a source. It also got
`requestHelpAnswer` and could point the answer contract anywhere.

`help/publicSurface.ts` is what remains: read the corpus, retrieve offline,
verify what a server returned, schedule and render. Authorization and answer
execution reach the host — the desktop through Tauri commands, the browser
through `GrokPtahBrokerClient.authorizeHelp` / `.answerHelp`.

*Proven by* `scripts/verify-public.mjs`, which asserts the absence of every
local-authority and local-transport symbol across all three bundles, that the
published corpus carries no non-public source, and that the packaged
`HelpRoute` is a named modal dialog with a live region and an inert background
that is not an ancestor of itself.

## Gate matrix

| Gate | Command | State |
| --- | --- | --- |
| Authority crate tests | `cargo test -p grokptah-help-authority` | 76 pass |
| Answer crate tests | `cargo test -p grokptah-help-answer` | 29 pass |
| Rust lint | `cargo clippy -p grokptah-help-{authority,answer} --all-targets -- -D warnings` | clean |
| Rust format | `cargo fmt -p grokptah-help-{authority,answer} -- --check` | clean |
| Desktop tests | `npm test` (in `desktop/`) | 536 pass |
| Typecheck | `npm run typecheck` | clean |
| Corpus / model / eval | `npm run help:verify` | thresholds met |
| Public package | `npm run verify:public` | clean, incl. packaged a11y |
| Tauri crate tests | `cargo test` (in `desktop/src-tauri/`) | 16 pass |
| Tauri crate lint | `cargo clippy --all-targets -- -D warnings` | **fails, not this lane** |

The last row: an unused `MacOsObservationPlatform` import in
`desktop/src-tauri/src/computer_use.rs`. That file is byte-identical to this
lane's parent commit and belongs to the Computer Use lane, so it is reported
rather than fixed here.

## Cross-implementation parity

Two fixtures bind implementations that must not drift:

- `crates/common/grokptah-help-authority/fixtures/authority-parity.json` — the
  same decision cases run by the Rust crate and the TypeScript mirror.
- `crates/common/grokptah-help-answer/fixtures/request-digest-parity.json` —
  the request digest an admission is minted over. A disagreement here would
  either fail every admission or verify one minted for a different body.
- `crates/common/grokptah-help-answer/fixtures/receipt-shape.json` — the exact
  receipt serialization, read by the broker client's parser. Regenerate with
  `cargo run -p grokptah-help-answer --example emit_receipt`.

## Deliberately not done

- **No product wiring.** `App.tsx`, `SettingsPanel.tsx`, and ContextDesk are
  untouched. The Tauri answer state is registered as *unconfigured*: there is
  no provider, and `help_answer_execute` returns `no-provider-configured`
  rather than a broken path. Offline Help search is fully useful in that state.
- **No live provider.** Nothing in this lane has been exercised against a real
  gateway. Every provider in the tests is a fixture.
- **No packaged desktop run.** The public package is built and verified; a
  signed desktop build is not part of this lane.
- **The broker endpoints are client-side only.** `authorizeHelp` and
  `answerHelp` are implemented and tested in the client; the server routes they
  call are not in this repository.

## Residual risks

1. **Vocabulary overlap is a proxy.** See the note under `claims.ts`. A quote
   that is topically right and factually contradictory passes coverage.
2. **The secret scan's `possible` tier is a heuristic.** It is calibrated so
   all 105 corpus chunks scan clean, and that calibration is a test. A future
   corpus addition could trip it, which is the intended failure direction.
3. **A deaf provider costs a worker permanently until it returns.** This is
   reported, not solved: an OS thread inside a blocking call cannot be killed
   safely. Capacity shrinks visibly rather than silently.
4. **Admissions are time-bounded, not revocable.** The 120-second ceiling is
   the whole revocation story within a window.
5. **Claim segmentation is English-shaped.** Sentence boundaries, abbreviation
   guards, and the stop-word list are tuned for the English corpus. Localized
   answers segment more coarsely.
