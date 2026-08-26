# Semantic Help

Help answers product questions from one corpus, offline, and can optionally ask
a provider for a written answer that the host checks against the corpus bytes
before anyone sees it.

This document records where authority lives and why, so a later change can tell
which properties are load-bearing.

## One corpus

There is exactly one hand-maintained Help corpus. It is authored as Rust seed
data in `grokptah-help-contract::data`, built into a digest-bound document, and
emitted as `desktop/src/lib/help/canonical/help-corpus.v1.json`. Rust embeds
that file with `include_str!`; the corpus is not maintained twice.

It replaced two corpora — `grokptah.help.v1` behind `HelpPanel` and
`product-corpus-v1` behind the old `HelpCenter` — which answered the same
questions differently with no way to tell which was current.

### Digests cover bytes, not names

Every record carries a digest over its own content:

| record  | digest covers |
|---------|---------------|
| source  | id, path, heading, visibility |
| chunk   | id, article, kind, ordinal, locale, **the chunk text**, visibility, sources |
| article | fields, visibility, aliases, keywords, capabilities, its sources' digests |
| corpus  | every article and chunk digest, in order, plus the source-set digest |

Two rules make these usable as evidence:

- **Length prefixing.** Fields encode as `<utf8_len>:<field>`. Joining with a
  separator is not injective — a separator inside a field makes two distinct
  field lists hash alike, which is a forgeable citation.
- **Domain separation.** The record kind is hashed first. A chunk id and a
  source id that happen to be the same string land in different digest spaces.

A citation span therefore commits to bytes. Rebuild the corpus with different
text and every span over it is invalidated rather than silently re-pointed.

`sha256`, the length-prefixed encoding, and the domain digests exist twice —
once in Rust, once in TypeScript, because a browser bundle cannot call Rust.
`generated/digest-parity.json` is emitted from the Rust side and asserted by the
TypeScript tests, including the cases a naive port gets wrong: multi-byte
characters, a field containing a separator, empty fields.

## One contract, generated

`grokptah-help-contract::codegen::model()` is the single description of every
authority, request, result, and receipt type. The JSON Schema
(`docs/schemas/grokptah-help.v1.schema.json`) and the TypeScript
(`desktop/src/lib/help/generated/contract.ts`) are both emitted from it, so they
cannot disagree with each other. `dto_tests::model_matches_rust_serde`
serializes a populated value of every modelled type and compares its JSON keys
to the model, so they cannot disagree with the Rust types either.

```sh
cargo run -p grokptah-help-contract --bin help-codegen -- --verify   # gate
cargo run -p grokptah-help-contract --bin help-codegen -- --write    # update
```

`--verify` re-emits every artifact and compares byte for byte. A hand edit to a
generated file fails the gate rather than surviving as a second source of truth.

## The renderer seam

A Tauri command can only accept what implements `Deserialize`. So the renderer's
entire inbound vocabulary is three types — `HelpAsk`, `HelpFollow`,
`HelpCancelRequest` — carrying a question, a locale, and opaque handles the host
issued. Grants, admissions, manifests, principals, requests, and receipts are
`Serialize`-only and therefore un-sendable **by construction**, not by
convention. `dto_tests::renderer_cannot_mint_authority` pins that set.

`HelpRequest` has no route, model, endpoint, tool, history, or workspace field.
A request that names its own route chose its own authority.

## Authorization is an action

The host holds the corpus, resolves the principal from a session token it
issued, and computes the manifest itself. **There is no entry point that accepts
a manifest.** An earlier design took the served index from its caller; a
decision made by the party it constrains is not a decision.

Between admitting an ask and serving its answer, the corpus can be rebuilt, the
manifest can move, the grant can expire, and access can be revoked. So the same
full check re-runs against current state at four points:

| checkpoint | about to |
|---|---|
| `Admission` | accept the ask at all |
| `QueuePromotion` | move it from waiting to running |
| `BeforeSend` | hand bytes to a provider |
| `BeforeServe` | show an answer to a renderer |

Six conditions deny, at every checkpoint, before any provider call: stale
revision, expiry, revocation, source drift, cross-tenant replay, substituted
request. `grokptah-help-authority` has no transport dependency, so a denial
cannot reach a provider even by accident; a test fails the build if one appears.

Every authorization outcome collapses to one public code, `not_available`, with
one fixed message. Distinguishing "revoked" from "no such source" would let a
caller map what exists by asking about it.

## The executor

`Provider` is `begin`/`poll`/`cancel` rather than a blocking `send`, because a
blocking call cannot express the distinction that matters on cancel: the
provider stopped, or the provider is not answering. `cancel` only *requests* a
stop; quiescence is learned from a later `poll`.

- A deaf provider leaves the run **`Abandoned`**, never `Cancelled`, and it
  **keeps its capacity slot** — the remote work may still be running, so the
  executor is genuinely one attempt short and says so.
- `begin` is called **at most once per run**. No retry, no second route, no
  fallback. Retrying would charge a caller who cancelled for a send they did
  not ask for.
- Send certainty is reported as observed: only an acknowledged begin yields
  `Sent`; an attempt that started without confirmation is `Unknown`, and neither
  cancel nor restart rewrites `Unknown` into `NotSent`.

Time is a parameter, not a clock read, so the executor is deterministic under
test. The clock is read once, at the Tauri edge.

## Validation

The provider returns prose. This side decides where claims begin and whether
corpus bytes support them — a model that labels its own citations is grading its
own work.

Support means a verbatim run of at least 24 characters found in a chunk the
request carried, yielding a span bound to that chunk's digest. Unsupported
claims are dropped; an answer with none left is an **abstention**, which is a
result, not an error and not a hedge.

Redaction removes what could act rather than inform: markup, control characters
that can rewrite an already-printed log, bidi overrides that reorder rendered
text without changing its bytes, and credentials or paths. Counts reach the
receipt; the removed text does not.

Receipts carry counts, digests, identities, and timings — never the question,
the answer, a chunk, or a reply. A log that quotes what it audits becomes a
second copy outside every control that governed the first.

## What the published package ships

`@grokptah/client` ships the offline half: the public corpus, retrieval,
verification, and rendering types. It does **not** ship authority constructors,
route selection, transport, raw provider replies, the private corpus, or the
executor.

TypeScript never imports `help-corpus.v1.json`. That is enforced, not intended:
an earlier version of the public surface imported its verifier from a module
that loaded the full corpus at the top level, and all 27 restricted chunks were
emitted into the published bundles while every export-level assertion still
passed. A bundler follows imports, not export lists.

Two gates cover it — `scripts/verify-public.mjs` searches the built bundles for
restricted *text*, and `helpBundle.test.ts` walks the import graph statically.

A principal entitled to more than the public set asks the host, which filters
and returns the corpus that principal may see. The renderer never filters: what
it holds is what it may see.

## What this is not

- **No provider is configured in this build.** `DesktopProvider::Unconfigured`
  returns `Begin::Rejected`, so every ask abstains after zero bytes leave the
  process. That is the honest state of a build with no qualified provider route,
  not a stub standing in for one. Offline retrieval does not depend on it.
- **Retrieval is lexical, not neural.** The "semantic" half is character-trigram
  cosine. It is deterministic and byte-stable across platforms, and it is not a
  pinned embedding model — shipping one would need a weights artifact whose
  provenance and licence this repository would have to qualify separately.
- **The gold set is small.** Fifteen positives and seven negatives, in
  `helpRetrieval.test.ts`. It is enough to have caught a real scoring defect; it
  is not a retrieval benchmark.
- **Nothing here is qualified for release.** No live provider campaign, no
  packaged build, no soak.

## Running the gates

```sh
# contract, corpus, authority, executor, validator
cargo test --locked -p grokptah-help-contract -p grokptah-help-authority -p grokptah-help-runtime
cargo run  --locked -p grokptah-help-contract --bin help-codegen -- --verify

# desktop surface, retrieval, adversarial, accessibility, bundle hygiene
cd desktop && npm run typecheck && npm test

# published package: exports, public-only corpus, consumer fixture
cd desktop && npm run verify:public

# Tauri host commands
cd desktop/src-tauri && cargo test --locked
```
