/* eslint-disable */
// GENERATED FILE - DO NOT EDIT.
// Rebuild: node --import ./scripts/register-ts-hook.mjs scripts/build-help-model.mjs
// Provenance, license, and checksum: ./provenance.ts and ./MODEL_PROVENANCE.md
export const HELP_MODEL_PROVENANCE_DATA = {
  "schema": "grokptah.help-model-provenance.v1",
  "modelId": "grokptah-help-lsa-v1",
  "modelVersion": "1.0.0",
  "artifact": "helpEmbeddingModel.v1.json",
  "sha256": "sha256:443d5e743c1d9f3131cb4acdda7e8e7049d73dfa566fedbf05cdc03cd6739233",
  "canonicalSerializationSha256": "443d5e743c1d9f3131cb4acdda7e8e7049d73dfa566fedbf05cdc03cd6739233",
  "method": "ppmi+truncated-svd(lsa)+char-trigram-subword",
  "dims": 64,
  "vocabularySize": 1527,
  "subwordCount": 1407,
  "chunkCount": 105,
  "documentCount": 231,
  "trainedFromCorpusDigest": "sha256:61f5cff166cf9efbdd2fdf0d51d3243687a9c922ac7d8e93cf7e027423ba5a71",
  "trainedFromContentVersion": "help-canonical-2026.08.1",
  "trainingInputs": "desktop/src/lib/help/canonical/data.ts plus the cited README.md and docs/*.md sections (this repository only)",
  "sourceParagraphs": 102,
  "externalData": "none",
  "network": "none — training and inference are fully offline",
  "runtime": "pure TypeScript; no native modules, no WASM, no model server",
  "license": "Apache-2.0 (same as this repository; the model is derived solely from repository content)",
  "redistributable": true,
  "buildCommand": "node --import ./scripts/register-ts-hook.mjs scripts/build-help-model.mjs",
  "verifyCommand": "node --import ./scripts/register-ts-hook.mjs scripts/verify-help-model.mjs",
  "determinism": {
    "svd": "cyclic one-sided Jacobi, fixed sweep order, epsilon 1e-12, ties broken by source index",
    "buildOps": "Math.log used only for IDF/PPMI weighting; values baked into the artifact",
    "runtimeOps": "add, subtract, multiply, divide, sqrt only — all correctly rounded under IEEE-754",
    "reproducedUnder": "node v22.22.2"
  }
} as const;
