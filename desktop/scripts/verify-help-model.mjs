/**
 * Verifies the pinned Help embedding model. Fails closed.
 *
 *  1. the artifact checksum matches its provenance record;
 *  2. the artifact is bound to the shipped corpus digest;
 *  3. rebuilding from source reproduces the artifact byte-for-byte;
 *  4. the generated provenance document matches the generated artifact;
 *  5. the model carries a redistributable license and claims no network use.
 *
 * The rebuild is run against a snapshot and the tree is restored afterwards,
 * so the check never leaves the working tree modified.
 */
import { readFile, writeFile } from "node:fs/promises";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const modelDir = join(here, "..", "src", "lib", "help", "model");
const generated = ["helpEmbeddingModel.v1.ts", "provenance.ts", "MODEL_PROVENANCE.md"];

const failures = [];
const fail = (message) => failures.push(message);

const { verifyHelpModelChecksum, HELP_MODEL_PROVENANCE, HELP_MODEL_STATS } = await import(
  "../src/lib/help/model/artifact.ts"
);
const { HELP_CORPUS } = await import("../src/lib/help/canonical/corpus.ts");

// 1. Checksum.
const checksum = verifyHelpModelChecksum();
if (!checksum.ok) fail(`model checksum mismatch: expected ${checksum.expected}, computed ${checksum.actual}`);

// 2. Corpus binding.
if (HELP_MODEL_STATS.corpusDigest !== HELP_CORPUS.digest) {
  fail(`model corpus digest ${HELP_MODEL_STATS.corpusDigest} != corpus ${HELP_CORPUS.digest}`);
}

// 3. Deterministic rebuild.
const snapshot = new Map();
for (const name of generated) snapshot.set(name, await readFile(join(modelDir, name), "utf8"));
const rebuild = spawnSync(
  process.execPath,
  ["--import", join(here, "register-ts-hook.mjs"), join(here, "build-help-model.mjs")],
  { cwd: join(here, ".."), encoding: "utf8" },
);
if (rebuild.status !== 0) {
  fail(`model rebuild failed: ${rebuild.stderr || rebuild.stdout}`);
} else {
  for (const name of generated) {
    const rebuilt = await readFile(join(modelDir, name), "utf8");
    if (rebuilt !== snapshot.get(name)) {
      fail(`rebuild is not byte-identical for ${name}; the checked-in artifact is stale or the build is non-deterministic`);
    }
  }
}
for (const [name, content] of snapshot) await writeFile(join(modelDir, name), content);

// 4. Provenance document agreement.
const markdown = snapshot.get("MODEL_PROVENANCE.md");
if (!markdown.includes(HELP_MODEL_PROVENANCE.sha256)) {
  fail("MODEL_PROVENANCE.md does not record the artifact checksum");
}
if (!markdown.includes(HELP_CORPUS.digest)) {
  fail("MODEL_PROVENANCE.md does not record the corpus digest");
}

// 5. License and offline claims must be explicit.
if (!/apache-2\.0/i.test(HELP_MODEL_PROVENANCE.license)) {
  fail(`model license is not the expected redistributable license: ${HELP_MODEL_PROVENANCE.license}`);
}
if (!/none/i.test(HELP_MODEL_PROVENANCE.network)) {
  fail(`model provenance does not assert an offline runtime: ${HELP_MODEL_PROVENANCE.network}`);
}

if (failures.length > 0) {
  console.error("Help embedding model verification FAILED:");
  for (const failure of failures) console.error(`  - ${failure}`);
  process.exit(1);
}
console.log(`Help embedding model verified: ${HELP_MODEL_PROVENANCE.modelId} v${HELP_MODEL_PROVENANCE.modelVersion}`);
console.log(`  method:    ${HELP_MODEL_PROVENANCE.method}`);
console.log(`  dims:      ${HELP_MODEL_STATS.dims}  vocab: ${HELP_MODEL_STATS.vocabularySize}  subwords: ${HELP_MODEL_STATS.subwordCount}`);
console.log(`  sha256:    ${HELP_MODEL_PROVENANCE.sha256}`);
console.log(`  license:   ${HELP_MODEL_PROVENANCE.license}`);
console.log(`  rebuild:   byte-identical`);
