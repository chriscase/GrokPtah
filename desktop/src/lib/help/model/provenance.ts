/* eslint-disable */
// GENERATED FILE - DO NOT EDIT.
// Rebuild: node --import ./scripts/register-ts-hook.mjs scripts/build-help-model.mjs
// Provenance, license, and checksum: ./provenance.ts and ./MODEL_PROVENANCE.md
export const HELP_MODEL_PROVENANCE_DATA = {
  "schema": "grokptah.help-model-provenance.v1",
  "modelId": "grokptah-help-lsa-v1",
  "modelVersion": "1.0.0",
  "artifact": "helpEmbeddingModel.v1.json",
  "sha256": "sha256:7a7edf68152634cf7266905d86d96f37d2b0ff950859f34d1eeb5bd95bbac869",
  "canonicalSerializationSha256": "7a7edf68152634cf7266905d86d96f37d2b0ff950859f34d1eeb5bd95bbac869",
  "method": "ppmi+truncated-svd(lsa)+char-trigram-subword",
  "dims": 64,
  "vocabularySize": 1527,
  "subwordCount": 1407,
  "chunkCount": 105,
  "documentCount": 231,
  "trainedFromCorpusDigest": "sha256:9547bad7a455554cb6b5b7419bea4b4e3739c0d66b082144f84bc14854e1b0d9",
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
