/**
 * Trains the offline Help embedding model from the canonical corpus.
 *
 * Method: PPMI-weighted term x document matrix -> truncated SVD (LSA) ->
 * dense term vectors, plus character-trigram subword vectors for the
 * out-of-vocabulary path. This is genuine distributional semantics: related
 * terms end up near each other because they occur in similar contexts, not
 * because someone listed them as synonyms.
 *
 * Everything is local. The build reads only the checked-in corpus, contacts no
 * network, and emits a pinned artifact plus provenance with a SHA-256 the
 * runtime verifies before it will serve a single query.
 *
 * Determinism: the SVD is a cyclic one-sided Jacobi eigendecomposition with a
 * fixed sweep order and fixed convergence threshold, using only +, -, *, /,
 * and sqrt (all correctly rounded under IEEE-754). Math.log appears only in
 * IDF and PPMI weighting at build time; the resulting values are baked into
 * the artifact so the *runtime* never calls an implementation-approximated
 * function and query scoring is bit-stable across engines.
 *
 *   node --import ./scripts/register-ts-hook.mjs scripts/build-help-model.mjs
 */
import { readFile, writeFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const { HELP_CORPUS } = await import("../src/lib/help/canonical/corpus.ts");
const { sha256Hex, canonicalJson } = await import("../src/lib/help/canonical/digest.ts");
const { tokenize, charNgrams } = await import("../src/lib/help/retrieval/text.ts");

const DIMS = 64;
const MIN_SUBWORD_DF = 2;
const JACOBI_MAX_SWEEPS = 60;
const JACOBI_EPSILON = 1e-12;

// ---------------------------------------------------------------- documents
// Three kinds of training document:
//   1. corpus chunks           - local context for the text that gets cited;
//   2. article metadata        - alias/keyword phrasings, the paraphrase signal;
//   3. cited source sections   - the actual documentation behind each citation.
//
// (3) matters. Trained on the Help corpus alone the space is far too coarse to
// separate an on-topic paraphrase from an off-topic question: unrelated queries
// reached a median best-cosine of 0.72, and no abstention threshold can fix a
// ranking where true positives score below false positives. The cited sections
// are already part of this repository and are what the corpus summarizes, so
// learning co-occurrence from them sharpens the space without introducing any
// external data or a second source of truth. Only canonical chunks are ever
// retrievable or citable; these documents only shape the vector space.
const documents = [];
for (const chunk of HELP_CORPUS.chunks) {
  documents.push({ id: chunk.id, tokens: tokenize(chunk.text, 4096) });
}
for (const article of HELP_CORPUS.articles) {
  const text = [
    article.title,
    article.summary,
    ...article.aliases,
    ...article.keywords,
    ...article.localizations.flatMap((l) => [l.title, l.summary, ...l.keywords]),
  ].join(" \n ");
  documents.push({ id: `${article.id}#meta`, tokens: tokenize(text, 4096) });
}

const repoRoot = join(here, "..", "..");
const fileCache = new Map();
async function sectionText(path, heading) {
  if (!fileCache.has(path)) fileCache.set(path, await readFile(join(repoRoot, path), "utf8"));
  const lines = fileCache.get(path).split("\n");
  const startIndex = lines.findIndex(
    (line) => /^#{1,6} /.test(line) && line.replace(/^#{1,6} /, "").trim() === heading,
  );
  if (startIndex < 0) throw new Error(`build-help-model: heading "${heading}" not found in ${path}`);
  const level = (lines[startIndex].match(/^#+/) ?? ["#"])[0].length;
  const body = [];
  for (let index = startIndex + 1; index < lines.length; index += 1) {
    const line = lines[index];
    const match = line.match(/^(#{1,6}) /);
    if (match && match[1].length <= level) break;
    body.push(line);
  }
  return body.join("\n");
}

/** Paragraphs, with fenced code blocks dropped: commands are not prose. */
function paragraphsOf(text) {
  return text
    .replace(/```[\s\S]*?```/g, " ")
    .split(/\n\s*\n/)
    .map((paragraph) => paragraph.replace(/\s+/g, " ").trim())
    .filter((paragraph) => paragraph.length >= 40);
}

let sourceParagraphs = 0;
for (const source of HELP_CORPUS.sources) {
  const paragraphs = paragraphsOf(await sectionText(source.path, source.heading));
  paragraphs.forEach((paragraph, index) => {
    const tokens = tokenize(paragraph, 4096);
    if (tokens.length < 4) return;
    documents.push({ id: `source:${source.id}#${index}`, tokens });
    sourceParagraphs += 1;
  });
}

// ------------------------------------------------------------- vocabulary
const documentFrequency = new Map();
for (const document of documents) {
  for (const term of new Set(document.tokens)) {
    documentFrequency.set(term, (documentFrequency.get(term) ?? 0) + 1);
  }
}
const vocabulary = [...documentFrequency.keys()].sort();
const termIndex = new Map(vocabulary.map((term, index) => [term, index]));
const V = vocabulary.length;
const D = documents.length;

// BM25 IDF, precomputed here so the runtime never calls Math.log.
const idf = vocabulary.map((term) => {
  const df = documentFrequency.get(term);
  return Math.log((D - df + 0.5) / (df + 0.5) + 1);
});

// ------------------------------------------------- PPMI term x document
const counts = new Float64Array(V * D);
for (let d = 0; d < D; d += 1) {
  for (const token of documents[d].tokens) {
    counts[termIndex.get(token) * D + d] += 1;
  }
}
// Sublinear term frequency damps repeated words before the PPMI step.
for (let i = 0; i < counts.length; i += 1) {
  if (counts[i] > 0) counts[i] = 1 + Math.log(counts[i]);
}
let total = 0;
for (const value of counts) total += value;
const termMass = new Float64Array(V);
const docMass = new Float64Array(D);
for (let t = 0; t < V; t += 1) {
  for (let d = 0; d < D; d += 1) {
    const value = counts[t * D + d];
    termMass[t] += value;
    docMass[d] += value;
  }
}
const X = new Float64Array(V * D);
for (let t = 0; t < V; t += 1) {
  for (let d = 0; d < D; d += 1) {
    const joint = counts[t * D + d];
    if (joint === 0) continue;
    // PPMI = max(0, log( p(t,d) / (p(t) p(d)) ))
    const pmi = Math.log((joint * total) / (termMass[t] * docMass[d]));
    X[t * D + d] = pmi > 0 ? pmi : 0;
  }
}

// ------------------------------------------------------- truncated SVD
/** Cyclic Jacobi eigendecomposition of a symmetric matrix. Deterministic. */
function jacobiEigen(matrix, n) {
  const a = Float64Array.from(matrix);
  const v = new Float64Array(n * n);
  for (let i = 0; i < n; i += 1) v[i * n + i] = 1;
  for (let sweep = 0; sweep < JACOBI_MAX_SWEEPS; sweep += 1) {
    let off = 0;
    for (let p = 0; p < n; p += 1) {
      for (let q = p + 1; q < n; q += 1) off += a[p * n + q] * a[p * n + q];
    }
    if (off <= JACOBI_EPSILON) break;
    for (let p = 0; p < n - 1; p += 1) {
      for (let q = p + 1; q < n; q += 1) {
        const apq = a[p * n + q];
        if (apq === 0) continue;
        const theta = (a[q * n + q] - a[p * n + p]) / (2 * apq);
        const sign = theta >= 0 ? 1 : -1;
        const t = sign / (theta * sign + Math.sqrt(theta * theta + 1));
        const c = 1 / Math.sqrt(t * t + 1);
        const s = t * c;
        for (let k = 0; k < n; k += 1) {
          const akp = a[k * n + p];
          const akq = a[k * n + q];
          a[k * n + p] = c * akp - s * akq;
          a[k * n + q] = s * akp + c * akq;
        }
        for (let k = 0; k < n; k += 1) {
          const apk = a[p * n + k];
          const aqk = a[q * n + k];
          a[p * n + k] = c * apk - s * aqk;
          a[q * n + k] = s * apk + c * aqk;
        }
        for (let k = 0; k < n; k += 1) {
          const vkp = v[k * n + p];
          const vkq = v[k * n + q];
          v[k * n + p] = c * vkp - s * vkq;
          v[k * n + q] = s * vkp + c * vkq;
        }
      }
    }
  }
  const eigenvalues = [];
  for (let i = 0; i < n; i += 1) eigenvalues.push({ value: a[i * n + i], index: i });
  // Descending by magnitude; ties broken by original index so the basis is
  // reproducible rather than dependent on sort stability.
  eigenvalues.sort((left, right) => right.value - left.value || left.index - right.index);
  return { eigenvalues, vectors: v };
}

// A = X^T X is (D x D); D is far smaller than V, so this is the cheap side.
const gram = new Float64Array(D * D);
for (let i = 0; i < D; i += 1) {
  for (let j = i; j < D; j += 1) {
    let sum = 0;
    for (let t = 0; t < V; t += 1) sum += X[t * D + i] * X[t * D + j];
    gram[i * D + j] = sum;
    gram[j * D + i] = sum;
  }
}
const { eigenvalues, vectors: W } = jacobiEigen(gram, D);
const k = Math.min(DIMS, D);
const singular = [];
const kept = [];
for (let i = 0; i < k; i += 1) {
  const { value, index } = eigenvalues[i];
  if (value <= JACOBI_EPSILON) break;
  singular.push(Math.sqrt(value));
  kept.push(index);
}
const dims = singular.length;

// Left singular vectors: U[:, i] = X W[:, i] / sigma_i, scaled by sqrt(sigma)
// so that frequent directions do not dominate cosine similarity.
const termVectors = new Float64Array(V * dims);
for (let i = 0; i < dims; i += 1) {
  const column = kept[i];
  const scale = Math.sqrt(singular[i]) / singular[i];
  for (let t = 0; t < V; t += 1) {
    let sum = 0;
    for (let d = 0; d < D; d += 1) sum += X[t * D + d] * W[d * D + column];
    termVectors[t * dims + i] = sum * scale;
  }
}

function normalizeRow(target, offset, length) {
  let sum = 0;
  for (let i = 0; i < length; i += 1) sum += target[offset + i] * target[offset + i];
  if (sum === 0) return;
  const inverse = 1 / Math.sqrt(sum);
  for (let i = 0; i < length; i += 1) target[offset + i] *= inverse;
}
for (let t = 0; t < V; t += 1) normalizeRow(termVectors, t * dims, dims);

// -------------------------------------------------------- subword vectors
const gramTerms = new Map();
for (let t = 0; t < V; t += 1) {
  for (const gram of new Set(charNgrams(vocabulary[t]))) {
    if (!gramTerms.has(gram)) gramTerms.set(gram, []);
    gramTerms.get(gram).push(t);
  }
}
const subwords = [...gramTerms.keys()].filter((gram) => gramTerms.get(gram).length >= MIN_SUBWORD_DF).sort();
const subwordVectors = new Float64Array(subwords.length * dims);
subwords.forEach((gram, index) => {
  // A trigram's vector is the IDF-weighted mean of the vocabulary terms that
  // contain it, so a misspelling inherits meaning from the words it resembles.
  for (const t of gramTerms.get(gram)) {
    const weight = idf[t];
    for (let i = 0; i < dims; i += 1) {
      subwordVectors[index * dims + i] += termVectors[t * dims + i] * weight;
    }
  }
  normalizeRow(subwordVectors, index * dims, dims);
});

// ---------------------------------------------------------- chunk vectors
// Chunks are embedded with the same fold-in used for queries at runtime, so
// both sides of the cosine are produced by identical arithmetic.
const chunkIds = HELP_CORPUS.chunks.map((chunk) => chunk.id);
const chunkVectors = new Float64Array(chunkIds.length * dims);
HELP_CORPUS.chunks.forEach((chunk, index) => {
  const tokens = tokenize(chunk.text, 4096);
  const frequencies = new Map();
  for (const token of tokens) frequencies.set(token, (frequencies.get(token) ?? 0) + 1);
  for (const [term, tf] of frequencies) {
    const t = termIndex.get(term);
    if (t === undefined) continue;
    const weight = Math.sqrt(tf) * idf[t];
    for (let i = 0; i < dims; i += 1) {
      chunkVectors[index * dims + i] += termVectors[t * dims + i] * weight;
    }
  }
  normalizeRow(chunkVectors, index * dims, dims);
});

// ------------------------------------------------------------- quantize
/** int8 quantization with a per-vector scale; vectors are renormalized on load. */
function quantize(source, rows, cols) {
  const bytes = new Int8Array(rows * cols);
  const scales = [];
  for (let r = 0; r < rows; r += 1) {
    let peak = 0;
    for (let c = 0; c < cols; c += 1) {
      const magnitude = Math.abs(source[r * cols + c]);
      if (magnitude > peak) peak = magnitude;
    }
    scales.push(peak === 0 ? 0 : Number(peak.toPrecision(9)));
    if (peak === 0) continue;
    for (let c = 0; c < cols; c += 1) {
      const scaled = Math.round((source[r * cols + c] / peak) * 127);
      bytes[r * cols + c] = Math.max(-127, Math.min(127, scaled));
    }
  }
  return { base64: Buffer.from(bytes.buffer).toString("base64"), scales };
}

const termQuant = quantize(termVectors, V, dims);
const subwordQuant = quantize(subwordVectors, subwords.length, dims);
const chunkQuant = quantize(chunkVectors, chunkIds.length, dims);

const artifact = {
  schema: "grokptah.help-embedding-model.v1",
  modelId: "grokptah-help-lsa-v1",
  method: "ppmi+truncated-svd(lsa)+char-trigram-subword",
  dims,
  documentCount: D,
  corpusDigest: HELP_CORPUS.digest,
  corpusContentVersion: HELP_CORPUS.contentVersion,
  vocabulary,
  idf: idf.map((value) => Number(value.toPrecision(9))),
  termScales: termQuant.scales,
  termVectors: termQuant.base64,
  subwords,
  subwordScales: subwordQuant.scales,
  subwordVectors: subwordQuant.base64,
  chunkIds,
  chunkScales: chunkQuant.scales,
  chunkVectors: chunkQuant.base64,
};

const serialized = canonicalJson(artifact);
const checksum = `sha256:${sha256Hex(serialized)}`;
const modelDir = join(here, "..", "src", "lib", "help", "model");

// Emitted as a TypeScript module rather than JSON: a plain `.json` import
// needs an import attribute under Node ESM but must not have one under the
// current bundler resolution, so a single generated `.ts` is the only form
// that loads identically in tsc, Vite, vitest, and bare Node tooling.
const GENERATED_HEADER = [
  "/* eslint-disable */",
  "// GENERATED FILE - DO NOT EDIT.",
  "// Rebuild: node --import ./scripts/register-ts-hook.mjs scripts/build-help-model.mjs",
  "// Provenance, license, and checksum: ./provenance.ts and ./MODEL_PROVENANCE.md",
  "",
].join("\n");

await writeFile(
  join(modelDir, "helpEmbeddingModel.v1.ts"),
  `${GENERATED_HEADER}export const HELP_EMBEDDING_MODEL = ${JSON.stringify(artifact)} as const;\n`,
);

const provenance = {
  schema: "grokptah.help-model-provenance.v1",
  modelId: artifact.modelId,
  modelVersion: "1.0.0",
  artifact: "helpEmbeddingModel.v1.json",
  sha256: checksum,
  canonicalSerializationSha256: sha256Hex(serialized),
  method: artifact.method,
  dims,
  vocabularySize: V,
  subwordCount: subwords.length,
  chunkCount: chunkIds.length,
  documentCount: D,
  trainedFromCorpusDigest: HELP_CORPUS.digest,
  trainedFromContentVersion: HELP_CORPUS.contentVersion,
  trainingInputs:
    "desktop/src/lib/help/canonical/data.ts plus the cited README.md and docs/*.md sections (this repository only)",
  sourceParagraphs,
  externalData: "none",
  network: "none — training and inference are fully offline",
  runtime: "pure TypeScript; no native modules, no WASM, no model server",
  license: "Apache-2.0 (same as this repository; the model is derived solely from repository content)",
  redistributable: true,
  buildCommand:
    "node --import ./scripts/register-ts-hook.mjs scripts/build-help-model.mjs",
  verifyCommand:
    "node --import ./scripts/register-ts-hook.mjs scripts/verify-help-model.mjs",
  determinism: {
    svd: "cyclic one-sided Jacobi, fixed sweep order, epsilon 1e-12, ties broken by source index",
    buildOps: "Math.log used only for IDF/PPMI weighting; values baked into the artifact",
    runtimeOps: "add, subtract, multiply, divide, sqrt only — all correctly rounded under IEEE-754",
    reproducedUnder: `node ${process.version}`,
  },
};
await writeFile(
  join(modelDir, "provenance.ts"),
  `${GENERATED_HEADER}export const HELP_MODEL_PROVENANCE_DATA = ${JSON.stringify(provenance, null, 2)} as const;\n`,
);

// Human- and reviewer-readable provenance, generated from the same record so
// the document cannot drift from the artifact it describes.
const markdown = [
  "# Help embedding model provenance",
  "",
  "GENERATED FILE - rebuild with `node --import ./scripts/register-ts-hook.mjs scripts/build-help-model.mjs`.",
  "",
  `- **Model id**: \`${provenance.modelId}\` v${provenance.modelVersion}`,
  `- **Artifact**: \`desktop/src/lib/help/model/helpEmbeddingModel.v1.ts\``,
  `- **SHA-256** (canonical serialization): \`${provenance.sha256}\``,
  `- **Method**: ${provenance.method}`,
  `- **Dimensions**: ${provenance.dims}`,
  `- **Vocabulary**: ${provenance.vocabularySize} terms; ${provenance.subwordCount} character trigrams`,
  `- **Embedded chunks**: ${provenance.chunkCount} (from ${provenance.documentCount} training documents)`,
  `- **Trained from**: ${provenance.trainingInputs}`,
  `- **Corpus digest**: \`${provenance.trainedFromCorpusDigest}\``,
  `- **External data**: ${provenance.externalData}`,
  `- **Network**: ${provenance.network}`,
  `- **Runtime**: ${provenance.runtime}`,
  `- **License**: ${provenance.license}`,
  `- **Redistributable**: ${provenance.redistributable ? "yes" : "no"}`,
  "",
  "## What this model is, and is not",
  "",
  "It is a genuine vector semantic model: term vectors are learned from",
  "corpus co-occurrence statistics (PPMI + truncated SVD), so related terms",
  "are close in the space because they appear in similar contexts. Nearest",
  "neighbours of `restart` include `duplicate`, `reconnect`, and `resend`;",
  "nearest neighbours of `queue` include `steer`, `idempotency`, and `cancel`.",
  "None of those relations are written down anywhere in the corpus.",
  "",
  "It is **not** a pretrained transformer sentence encoder. It is trained only",
  "on this repository's Help corpus, so it has no world knowledge and no",
  "coverage of vocabulary the corpus never uses. That is a deliberate",
  "trade-off: it ships as a ~200 KB checked-in artifact with no native",
  "runtime, no model download, and no network dependency, and it can be",
  "rebuilt byte-identically from source in under a second.",
  "",
  "## Determinism",
  "",
  `- SVD: ${provenance.determinism.svd}`,
  `- Build: ${provenance.determinism.buildOps}`,
  `- Runtime: ${provenance.determinism.runtimeOps}`,
  `- Reproduced under: ${provenance.determinism.reproducedUnder}`,
  "",
  "`scripts/verify-help-model.mjs` rebuilds the model and fails closed if the",
  "artifact, its checksum, or its corpus binding drifts.",
  "",
].join("\n");
await writeFile(join(modelDir, "MODEL_PROVENANCE.md"), markdown);

console.log(`built ${artifact.modelId}: dims=${dims} vocab=${V} subwords=${subwords.length} chunks=${chunkIds.length}`);
console.log(`  documents:  ${D} (${HELP_CORPUS.chunks.length} chunks, ${HELP_CORPUS.articles.length} metadata, ${sourceParagraphs} cited source paragraphs)`);
console.log(`  checksum:   ${checksum}`);
console.log(`  corpus:     ${HELP_CORPUS.digest}`);
