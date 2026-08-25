# Help semantic core

The canonical Help corpus, its offline hybrid retriever, the pinned local
embedding model, and the bounded Help-answer contract.

Everything here is offline. Search needs no provider, no network, no model
download, and no configuration.

## What changed, and why

Two Help corpora shipped side by side:

| | `desktop/src/lib/help.ts` | `desktop/src/lib/helpCenter.ts` |
|---|---|---|
| Contract | `grokptah.help.v1` | `product-corpus-v1` |
| Entries | 10 | 19 |
| Source citations | none | yes |
| Capability / audience / access | yes | none |

Content was duplicated between them, and each could drift independently. Both
are now generated projections of one hand-maintained corpus at
`desktop/src/lib/help/canonical/data.ts`. Their published shapes, exports, and
ranking behavior are unchanged.

Three things the merge fixed:

- **Ambiguous citation ids.** `product.readme`, `provider.profiles`, and
  `computer-use.threat-model` each pointed at two different headings, so a
  citation could not identify which section backed a claim. Ids are now unique
  per `path#heading`.
- **Uncited content.** The five entry-corpus articles with no citations gained
  real source anchors and joined the article corpus (19 → 24 articles).
- **Order.** Canonical order is topic-then-authored-order. Sorting by id would
  lead with `computer-use` and, inside `getting-started`, with accessibility
  rather than the introductory article.

The entry projection stays scoped to its ten published entries. Widening it
silently reordered results — the not-yet-qualified isolated-guest article
outranked the Computer Use safety article under that contract's substring
scorer — and a refactor should not change a published contract's output.

## Canonical corpus

- 24 articles, 105 chunks, 22 unique source anchors.
- Chunk ids are `articleId#locale.kind.ordinal` — stable across rebuilds and
  citable verbatim by the answer contract.
- Every article carries at least one anchor that resolves to a real repository
  path *and* a real heading.
- Canonical serialization sorts object keys, so structurally equal content
  always digests identically.
- `corpus.lock.json` pins the digest, source digest, and counts. Drift has to
  be a reviewed change.

`npm run help:verify-corpus` fails closed on a missing file, a missing heading,
an ambiguous citation id, a duplicate or over-long chunk, a reintroduced
hand-maintained corpus array, or a secret pattern in shipped text.

## Embedding model

`grokptah-help-lsa-v1` — PPMI-weighted term × document matrix, truncated SVD
(LSA), plus character-trigram subword vectors.

| | |
|---|---|
| Dimensions | 64 |
| Vocabulary | 1,527 terms; 1,407 trigrams |
| Training documents | 231 (105 chunks, 24 article metadata, 102 cited source paragraphs) |
| Artifact | `desktop/src/lib/help/model/helpEmbeddingModel.v1.ts`, ~200 KB |
| License | Apache-2.0, same as this repository |
| External data | none |
| Network | none, at build or query time |
| Runtime | pure TypeScript; no native module, no WASM, no model server |

Checksum, license, and full provenance: `desktop/src/lib/help/model/`
(`provenance.ts`, `MODEL_PROVENANCE.md`).

**This is genuine vector semantics, not a synonym table.** Term vectors are
learned from co-occurrence, so related terms are close because they appear in
similar contexts. Nearest neighbours of `restart` are duplicate, reconnect,
resend; of `queue` are steer, idempotency, cancel; of `screen` are keyboard,
reader, contrast. None of those relations are written down anywhere.

**It is not a pretrained transformer encoder.** It is trained only on this
repository, so it has no world knowledge and no coverage of vocabulary the
corpus never uses. That is the deliberate trade: a checked-in artifact with no
download, no native runtime, and no network, rebuildable byte-identically in
under two seconds.

Training on the cited README/docs sections — not just the corpus — was
necessary, not incidental. On the Help corpus alone the space was too coarse to
separate an on-topic paraphrase from an off-topic question: unrelated queries
reached a median best-cosine of 0.72, and no threshold can repair a ranking
where true positives score below false positives.

### Determinism

- SVD is a cyclic one-sided Jacobi eigendecomposition with a fixed sweep order,
  a fixed epsilon, and ties broken by source index.
- `Math.log` appears only in build-time IDF/PPMI weighting; those values are
  baked into the artifact, so **query scoring never calls an
  implementation-approximated function** and is bit-stable across engines.
- `npm run help:verify-model` rebuilds against a snapshot, compares bytes, and
  restores the tree.

## Retrieval

Three separately reportable signals, fused on a common 0–1 scale:

| Component | Weight | What it measures |
|---|---|---|
| BM25 (lexical) | 0.67 | term overlap over chunks and article metadata |
| Semantic | 0.25 | cosine in the embedding space, discounted by query familiarity |
| Exact phrase | 0.08 | verbatim whole-query occurrence |

Two modifiers, both of which came out of measurement:

- **Coordination** (share of query terms an article matches), damped by an
  exponent of 0.15. Undamped it suppressed single-rare-term false matches but
  punished long paraphrases just as hard — `convert 40 celsius to fahrenheit`
  had been carried by an article containing the word "converts".
- **Query familiarity** — how much of the query the corpus can account for —
  scales the semantic component. Without it the subword backoff hands unknown
  words a plausible vector and `how do I bake sourdough bread` scored high
  enough to answer.

Scores are absolute, not per-query normalized. Normalizing by the per-query
maximum forces the top hit to 1.0 for every query including nonsense ones,
which makes a calibrated abstention threshold impossible.

Misspellings are corrected against the vocabulary *before* retrieval, using
Damerau distance (transposition is one edit) with a common-prefix guard.
Without the guard the vocabulary swallows ordinary English — `please` became
`lease`, `contents` became `consent` — handing off-topic queries lexical
evidence. Short words are never corrected.

Other properties: results are bounded in query length, term count, and count;
ties break on article id (a total order) so ranking is reproducible; the
outcome is bound to the corpus digest and fails closed when a caller pins a
digest that no longer matches; cancellation is cooperative.

### Abstention

Abstain below a fused score of **0.38**, chosen by an offline grid sweep over
weights, damping, and threshold, maximizing Recall@1 subject to a false-answer
rate at or below 5%. Reproduce with `npm run help:eval`.

The frontier is real. Nearby operating points:

| False answers | Answerable declined | Recall@1 |
|---|---|---|
| 0% | 13% | 82.4% |
| **3.7%** | **8.4%** | **84.9%** |
| 11% | 5% | 84.9% |

Driving false answers to zero costs about five more points of coverage. The
chosen point declines roughly one answerable query in twelve rather than
inventing support for one that has none.

## Measured quality

146 gold queries (119 answerable, 27 must-abstain) at
`desktop/src/lib/help/eval/goldset.ts`. Expected articles were chosen by
reading the corpus, not by recording what the retriever returned.

| Metric | Measured | Gate |
|---|---|---|
| Recall@1 (top-1 relevant) | 84.9% | ≥ 80% |
| Top-1 exact article | 78.2% | ≥ 74% |
| Recall@3 | 91.6% | ≥ 88% |
| MRR | 87.8% | ≥ 84% |
| Citation accuracy | 100% | 100% |
| False-answer rate | 3.7% | ≤ 6% |
| Answerable abstention | 8.4% | ≤ 12% |
| Abstention recall on unsupported | 96.3% | — |

By category (answerable only):

| Category | n | Recall@1 | Recall@3 |
|---|---|---|---|
| exact | 15 | 100% | 100% |
| misspelling | 12 | 100% | 100% |
| multilingual | 16 | 100% | 100% |
| expert | 36 | 86.1% | 97.2% |
| adversarial | 5 | 80% | 100% |
| paraphrase | 32 | 68.8% | 75% |
| secret | 3 | 33.3% | 66.7% |

Recall@1 is reported two ways because several queries are genuinely answered by
either of two articles — a question about idempotency is covered by both
durable recovery and the prompt queue — so crediting one id understates
ranking quality. The stricter exact-article rate is reported alongside it.

**Paraphrase is the weakest class and the honest limit of a corpus-trained
model.** A query like "my app crashed halfway through, how do I pick up where
it left off" shares almost no vocabulary with the corpus; the relevant article
is usually retrieved, but at rank 2–3. A pretrained sentence encoder would do
better here, at the cost of the offline, no-download, no-native-runtime
properties above.

## Bounded answer contract

`grokptah.help-answer.v1` is deliberately **not** ordinary Chat: one
request/response exchange with no tools, no history, no persistence, no
workspace access, and no provider fallback. It only phrases an answer over
chunks that offline retrieval already selected. With no provider configured it
reports `no-provider-configured` and the caller keeps its offline results.

Fails closed, never repairs: unknown key, stale corpus digest, mutated route,
empty or oversized answer, missing uncertainty, missing or excess citations, a
citation outside the supplied context, a citation whose source does not back
its chunk, markup, or a credential pattern.

The route (provider, tenant, model) is frozen and digest-bound. A reply naming
a different route is rejected — silently answering from a route the user did
not confirm is the substitution this contract exists to prevent. Transport
errors report only the error's type name, never a provider message that could
carry a URL, header, or credential. Timeout and cancellation share one cleanup
path.

## Security and accessibility

Credentials and private paths are redacted **before** tokenization, so a
pasted key is never indexed, scored, echoed in an excerpt, or forwarded. Only
the kind and count of what was removed is reported.

Highlights are offset ranges over plain text, never markup. No primitive
builds an HTML string and nothing assigns `innerHTML`, so there is no path for
corpus or provider text to inject markup. Control, zero-width, and
bidi-override characters are stripped from anything rendered.

Primitives carry state in ARIA and text rather than color (forced-colors
safe), use a real `<label>`, expose a polite live region for counts,
corrections, and redaction notices, are fully keyboard operable, and set no
fixed pixel sizing, `overflow: hidden`, or `nowrap` that would clip at 200%
text.

## Consuming it

```ts
// Headless — no React, no Tauri, no dependencies.
import { searchHelpCorpus, createHelpSearchController } from "@grokptah/client/ui-core";

// React primitives — React is an optional peer.
import { HelpResults, HelpSearchInput, useHelpSearch } from "@grokptah/client/help-react";
```

`npm run verify:public` builds the package, checks the exports and bundle
boundaries, then installs the generated tarball into a disposable fixture and
imports it through real `node_modules` resolution.

## Commands

```bash
npm run help:verify        # corpus + model + eval
npm run help:verify-corpus # anchors, digest lock, chunk bounds, secrets
npm run help:verify-model  # checksum, corpus binding, byte-identical rebuild
npm run help:eval          # retrieval metrics against the gold set
npm run help:build-model   # rebuild the artifact after a corpus change
```

After editing the corpus, run `help:build-model` then
`help:verify-corpus -- --write`; the model is bound to the corpus digest and
throws at import if the corpus moved without a rebuild.

## Authority

Help is a capability with a real boundary, not a retrieval library with
advisory filters. `crates/common/grokptah-help-authority` decides every
search, source read, and bounded answer **at the moment of the action**,
against the principal, tenant, project, and capability supplied with that
action — never a cached session decision.

- **Default deny.** Non-public sources require an explicit grant naming the
  same tenant and scope. A `project` source with no project id denies as
  `malformed_scope`; it does not fall back to public.
- **Tenant before scope.** A cross-tenant probe cannot learn that a project id
  exists from the difference between two denials.
- **Closed contracts.** Every request type is `deny_unknown_fields`. A field
  this version does not understand is a rejection, because a silently dropped
  restriction is how default-deny becomes allow-by-omission.
- **Non-leaking receipts.** A receipt carries ids and digests only, asserted
  by tests that scan a serialized receipt for paths, headings, content, and
  query text. It has no capability field at all, so there is nowhere for a
  grant to be recorded even by accident.

### Two implementations, one rule set

The desktop uses the Rust crate; the browser broker and embedding consumers
use a TypeScript mirror. Both execute the **same 25 fixtures**
(`crates/common/grokptah-help-authority/fixtures/authority-parity.json`) and
must agree case for case. The TypeScript bounds count UTF-8 bytes, matching
Rust, because counting UTF-16 code units would let an oversized identifier
through on one side only.

Both transports are single delegations to `authorize_for_served`, which lives
in the authority crate where it is tested. Neither adapter holds logic that
could drift.

The JSON Schema is emitted from the same constants as the Rust structs and
compared by parsed equality, not bytes: other workspace crates enable
`serde_json/preserve_order`, so key order differs between a whole-workspace
build and a `-p` build of this crate.

## Digests

Domain-separated and length-prefixed. The earlier source digest joined
`id|path#heading`, which collides whenever a separator appears inside a field
— `["a|b","c"]` and `["a","b|c"]` produced identical input. Every article,
chunk, and source now carries its own digest in its own domain, and the corpus
lock records them individually so a single edited record appears in the diff
rather than only shifting one aggregate.

The **index digest** binds the corpus, model, tokenizer, and every scoring
constant. Individually correct digests could not detect a scoring-parameter
change; this one can, and `expectIndexDigest` fails closed on it.

## Claim-level citations

A citation naming only an article is not checkable. Every citation carries the
exact quote plus offsets re-derived from the corpus during validation, so a
consumer can verify a claim without trusting the producer.

- **Two coordinate systems.** UTF-16 offsets for JavaScript slicing and code
  points for consumers that are not UTF-16, because a naive offset splits an
  emoji.
- **NFC everywhere.** Corpus text is normalized at construction so spans live
  in one coordinate system; a quote supplied as NFD still matches and is
  stored back in the corpus's form.
- **Mapped sanitization.** Removing zero-width and bidi characters shifts
  every later offset, so start and end are tracked per unit. An earlier version
  read the exclusive end from the next entry's start and swallowed collapsed
  whitespace — a span over "runs" reported "runs ".
- **Abstain on incomplete support.** An answer ranging far beyond what it
  quoted is rejected as `insufficient-support` rather than shown partially
  supported.

## Task runtime

Every Help task carries a deadline. Cancellation settles the caller
immediately whether or not the work observes its signal, so work that ignores
its signal wastes CPU but can never wedge the queue. The queue is bounded
rather than growing with a burst of keystrokes, and shutdown drains in bounded
time so a restart never inherits a half-finished task. One settle-once guard
covers the timeout/cancel/completion race, and work cancelled before its first
microtask is never invoked at all.

## Help route

A dedicated searchable route and palette. The background is marked `inert`,
not merely `aria-hidden`, because `aria-hidden` alone leaves it tabbable.
Focus returns to the opener on every close path. One polite live region
carries counts, corrections, redaction notices, and provider state. There is
no inline color, transition, fixed pixel sizing, `overflow: hidden`, or
`nowrap`, so forced-colors, reduced-motion, 400% zoom, and narrow viewports
lose no information.

The offline case is stated as a fact, not an error: with no provider
configured, search is the product rather than a degraded mode.

## Gate status on this branch

| Gate | Result |
|---|---|
| `help:verify-corpus` | PASS |
| `help:verify-model` | PASS, byte-identical rebuild |
| `help:eval` | PASS, every metric above threshold |
| `typecheck` | PASS |
| `test` | PASS, 474 tests / 59 files |
| `build` | PASS |
| `verify:public` | PASS, including packaged consumer smoke |
| `cargo fmt -p grokptah-help-authority --check` | PASS |
| `cargo clippy -p grokptah-help-authority -- -D warnings` | PASS |
| `cargo test -p grokptah-help-authority --locked` | PASS, 22 tests |
| `git diff --check` | PASS |

### The desktop shell gate needs a pre-existing fix first

`cargo test --locked` in `desktop/src-tauri` **does not build at this base**,
for a reason unrelated to this branch: `crates/common/grokptah-agent-sdk`
fails `#![deny(missing_docs)]` at `src/run.rs:230` under rustc 1.92.0. This
branch never touches that crate, and the same failure reproduces from the root
workspace on an unmodified tree.

To verify the Tauri adapter anyway, that one missing doc comment was patched
**locally and not committed**, the shell was built, and all 13 of its tests
passed with `src/help.rs` and its handler registration compiled in. The patch
was then reverted. The gate cannot run in CI until that pre-existing break is
fixed separately.

## Not covered by this lane

- Packaged desktop or macOS identity evidence.
- Any live-provider campaign. The answer contract has no live-route receipt;
  only its schema, bounds, and failure modes are covered by tests.
- ContextDesk product wiring. The primitives are exported and packaged, but no
  product page consumes them here.
- Corpus coverage beyond the 24 articles. Questions outside them abstain by
  design, which is correct but not the same as answering them.
