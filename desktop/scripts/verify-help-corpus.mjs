/**
 * Canonical Help corpus verification. Fails closed.
 *
 * Checks, against the exact checked-out tree:
 *  1. every cited source anchor resolves to a real path AND a real heading;
 *  2. the committed digest lock still matches the rebuilt corpus;
 *  3. chunk IDs are unique, bounded, and every chunk carries a citation;
 *  4. the two legacy contracts are projections, not second corpora;
 *  5. no corpus text leaks a secret marker or an absolute private path.
 */
import { readFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join, resolve } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");

const { HELP_CORPUS, recomputeHelpCorpusDigest, serializeHelpCorpus } = await import(
  "../src/lib/help/canonical/corpus.ts"
);
const { sha256Hex } = await import("../src/lib/help/canonical/digest.ts");
const { PROJECTED_HELP_ARTICLES, PROJECTED_HELP_ENTRIES } = await import(
  "../src/lib/help/canonical/projections.ts"
);

const failures = [];
const fail = (message) => failures.push(message);

// 1. Source anchors must resolve to a real path and a real heading.
const headingCache = new Map();
async function headingsFor(path) {
  if (!headingCache.has(path)) {
    const absolute = join(repoRoot, path);
    if (!existsSync(absolute)) { headingCache.set(path, null); return null; }
    const text = await readFile(absolute, "utf8");
    headingCache.set(
      path,
      new Set(
        text
          .split("\n")
          .filter((line) => /^#{1,6} /.test(line))
          .map((line) => line.replace(/^#{1,6} /, "").trim()),
      ),
    );
  }
  return headingCache.get(path);
}

for (const source of HELP_CORPUS.sources) {
  const headings = await headingsFor(source.path);
  if (headings === null) { fail(`source ${source.id}: missing file ${source.path}`); continue; }
  if (!headings.has(source.heading)) {
    fail(`source ${source.id}: heading "${source.heading}" not found in ${source.path}`);
  }
}

// A citation id must identify exactly one path#heading.
const byId = new Map();
for (const source of HELP_CORPUS.sources) {
  const target = `${source.path}#${source.heading}`;
  if (byId.has(source.id) && byId.get(source.id) !== target) {
    fail(`source id ${source.id} is ambiguous: ${byId.get(source.id)} vs ${target}`);
  }
  byId.set(source.id, target);
}

// 2. Digest lock. Drift must be an explicit, reviewed change.
const lockPath = join(here, "..", "src", "lib", "help", "canonical", "corpus.lock.json");
const rebuilt = {
  schemaVersion: HELP_CORPUS.schemaVersion,
  contentVersion: HELP_CORPUS.contentVersion,
  digest: HELP_CORPUS.digest,
  sourceDigest: HELP_CORPUS.sourceDigest,
  articleCount: HELP_CORPUS.articles.length,
  chunkCount: HELP_CORPUS.chunks.length,
  sourceCount: HELP_CORPUS.sources.length,
  serializationSha256: sha256Hex(serializeHelpCorpus()),
};
if (process.argv.includes("--write")) {
  const { writeFile } = await import("node:fs/promises");
  await writeFile(lockPath, `${JSON.stringify(rebuilt, null, 2)}\n`);
  console.log("wrote corpus lock:", rebuilt.digest);
} else if (!existsSync(lockPath)) {
  fail("corpus.lock.json is missing; run with --write to create it");
} else {
  const locked = JSON.parse(await readFile(lockPath, "utf8"));
  for (const key of Object.keys(rebuilt)) {
    if (locked[key] !== rebuilt[key]) {
      fail(`corpus lock drift on ${key}: locked ${locked[key]} != rebuilt ${rebuilt[key]}`);
    }
  }
}

// Recomputing the digest from the serialization must reproduce it exactly.
//
// Through the exported path, not a copy of the rules: a verifier that
// re-implements the record rules can agree with a corpus that has drifted,
// because both sides drifted together.
if (recomputeHelpCorpusDigest(serializeHelpCorpus()) !== HELP_CORPUS.digest) {
  fail("canonical digest is not reproducible from the published serialization");
}

// Every record carries its own digest, and a citation span binds to the chunk's.
for (const chunk of HELP_CORPUS.chunks) {
  if (!/^sha256:[0-9a-f]{64}$/.test(chunk.digest ?? "")) {
    fail(`chunk ${chunk.id} has no digest; citation spans would bind to nothing`);
  }
}
for (const source of HELP_CORPUS.sources) {
  if (!/^sha256:[0-9a-f]{64}$/.test(source.digest ?? "")) {
    fail(`source ${source.id} has no digest`);
  }
}

// 3. Chunk invariants.
const chunkIds = new Set();
for (const chunk of HELP_CORPUS.chunks) {
  if (chunkIds.has(chunk.id)) fail(`duplicate chunk id ${chunk.id}`);
  chunkIds.add(chunk.id);
  if (chunk.text.length === 0) fail(`empty chunk ${chunk.id}`);
  if (chunk.text.length > 512) fail(`chunk ${chunk.id} exceeds 512 characters`);
  if (chunk.sourceIds.length === 0) fail(`chunk ${chunk.id} has no citation`);
  for (const sourceId of chunk.sourceIds) {
    if (!byId.has(sourceId)) fail(`chunk ${chunk.id} cites unknown source ${sourceId}`);
  }
  if (!HELP_CORPUS.articles.some((article) => article.id === chunk.articleId)) {
    fail(`chunk ${chunk.id} references unknown article ${chunk.articleId}`);
  }
}

// 4. The legacy contracts must be projections of this corpus, not a second one.
for (const [file, marker] of [
  ["../src/lib/help.ts", "PROJECTED_HELP_ENTRIES"],
  ["../src/lib/helpCenter.ts", "PROJECTED_HELP_ARTICLES"],
]) {
  const text = await readFile(join(here, file), "utf8");
  if (!text.includes(marker)) fail(`${file} no longer consumes the canonical projection ${marker}`);
  if (/^const HELP_(ARTICLE|ENTRY)_DATA/m.test(text)) {
    fail(`${file} reintroduced a hand-maintained corpus array`);
  }
}
if (PROJECTED_HELP_ARTICLES.length !== HELP_CORPUS.articles.length) {
  fail("article projection lost articles");
}
for (const entry of PROJECTED_HELP_ENTRIES) {
  const article = HELP_CORPUS.articles.find((item) => item.legacyEntryId === entry.id);
  if (!article) fail(`entry projection ${entry.id} has no canonical article`);
  else if (article.body !== entry.body) fail(`entry projection ${entry.id} body drifted from canonical`);
}

// 5. No secret markers or absolute private paths in shipped corpus text.
const secretPatterns = [
  /\bxai-[A-Za-z0-9]{8,}/i,
  /\bsk-[A-Za-z0-9]{16,}/,
  /Authorization:\s*Bearer\s+\S+/i,
  /XAI_API_KEY\s*=/,
  /-----BEGIN [A-Z ]*PRIVATE KEY-----/,
  /\/Users\/[A-Za-z0-9._-]+\//,
  /\/home\/[A-Za-z0-9._-]+\//,
  /\/private\/(?:var|tmp|etc)\//,
  /\bGROKPTAH_HOME\b/,
];
for (const article of HELP_CORPUS.articles) {
  const blob = [article.title, article.summary, article.body, ...article.aliases, ...article.keywords].join("\n");
  for (const pattern of secretPatterns) {
    if (pattern.test(blob)) fail(`article ${article.id} matches forbidden pattern ${pattern}`);
  }
}

if (failures.length > 0) {
  console.error("canonical Help corpus verification FAILED:");
  for (const failure of failures) console.error(`  - ${failure}`);
  process.exit(1);
}
console.log(
  `canonical Help corpus verified: ${HELP_CORPUS.articles.length} articles, ` +
    `${HELP_CORPUS.chunks.length} chunks, ${HELP_CORPUS.sources.length} sources`,
);
console.log(`  digest:       ${HELP_CORPUS.digest}`);
console.log(`  sourceDigest: ${HELP_CORPUS.sourceDigest}`);
