# Help embedding model provenance

GENERATED FILE - rebuild with `node --import ./scripts/register-ts-hook.mjs scripts/build-help-model.mjs`.

- **Model id**: `grokptah-help-lsa-v1` v1.0.0
- **Artifact**: `desktop/src/lib/help/model/helpEmbeddingModel.v1.ts`
- **SHA-256** (canonical serialization): `sha256:443d5e743c1d9f3131cb4acdda7e8e7049d73dfa566fedbf05cdc03cd6739233`
- **Method**: ppmi+truncated-svd(lsa)+char-trigram-subword
- **Dimensions**: 64
- **Vocabulary**: 1527 terms; 1407 character trigrams
- **Embedded chunks**: 105 (from 231 training documents)
- **Trained from**: desktop/src/lib/help/canonical/data.ts plus the cited README.md and docs/*.md sections (this repository only)
- **Corpus digest**: `sha256:61f5cff166cf9efbdd2fdf0d51d3243687a9c922ac7d8e93cf7e027423ba5a71`
- **External data**: none
- **Network**: none — training and inference are fully offline
- **Runtime**: pure TypeScript; no native modules, no WASM, no model server
- **License**: Apache-2.0 (same as this repository; the model is derived solely from repository content)
- **Redistributable**: yes

## What this model is, and is not

It is a genuine vector semantic model: term vectors are learned from
corpus co-occurrence statistics (PPMI + truncated SVD), so related terms
are close in the space because they appear in similar contexts. Nearest
neighbours of `restart` include `duplicate`, `reconnect`, and `resend`;
nearest neighbours of `queue` include `steer`, `idempotency`, and `cancel`.
None of those relations are written down anywhere in the corpus.

It is **not** a pretrained transformer sentence encoder. It is trained only
on this repository's Help corpus, so it has no world knowledge and no
coverage of vocabulary the corpus never uses. That is a deliberate
trade-off: it ships as a ~200 KB checked-in artifact with no native
runtime, no model download, and no network dependency, and it can be
rebuilt byte-identically from source in under a second.

## Determinism

- SVD: cyclic one-sided Jacobi, fixed sweep order, epsilon 1e-12, ties broken by source index
- Build: Math.log used only for IDF/PPMI weighting; values baked into the artifact
- Runtime: add, subtract, multiply, divide, sqrt only — all correctly rounded under IEEE-754
- Reproduced under: node v22.22.2

`scripts/verify-help-model.mjs` rebuilds the model and fails closed if the
artifact, its checksum, or its corpus binding drifts.
