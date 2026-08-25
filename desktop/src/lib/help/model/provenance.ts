/* eslint-disable */
// GENERATED FILE - DO NOT EDIT.
// Rebuild: node --import ./scripts/register-ts-hook.mjs scripts/build-help-model.mjs
// Provenance, license, and checksum: ./provenance.ts and ./MODEL_PROVENANCE.md
export const HELP_MODEL_PROVENANCE_DATA = {
  "schema": "grokptah.help-model-provenance.v1",
  "modelId": "grokptah-help-lsa-v1",
  "modelVersion": "1.0.0",
  "artifact": "helpEmbeddingModel.v1.json",
  "sha256": "sha256:c26d3af8cdf9cb8ac35eaae6df4d7eebd60a3c2e948cd8a3295c349c59378523",
  "canonicalSerializationSha256": "c26d3af8cdf9cb8ac35eaae6df4d7eebd60a3c2e948cd8a3295c349c59378523",
  "method": "ppmi+truncated-svd(lsa)+char-trigram-subword",
  "dims": 64,
  "vocabularySize": 809,
  "subwordCount": 921,
  "chunkCount": 105,
  "documentCount": 129,
  "trainedFromCorpusDigest": "sha256:add280ca1f15e19e7188f3b4498a2e0c1d61209aee3bbd47f89dfeae38f7e06e",
  "trainedFromContentVersion": "help-canonical-2026.08.1",
  "trainingInputs": "desktop/src/lib/help/canonical/data.ts (this repository only)",
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
