/**
 * Loader for the pinned offline Help embedding model.
 *
 * The artifact is checked in (see `provenance.json` for method, license, and
 * SHA-256) and is trained only from this repository's canonical corpus. No
 * network access, no native module, and no model server is involved at build
 * or query time.
 *
 * The model is bound to the corpus digest it was trained from. A corpus edit
 * without a model rebuild fails closed at import rather than silently serving
 * stale vectors.
 */
import { HELP_CORPUS_DIGEST } from "../canonical/corpus";
import { canonicalJson, sha256Hex } from "../canonical/digest";
import { charNgrams } from "../retrieval/text";
import { HELP_EMBEDDING_MODEL } from "./helpEmbeddingModel.v1";
import { HELP_MODEL_PROVENANCE_DATA } from "./provenance";

export type HelpModelProvenance = {
  readonly modelId: string;
  readonly modelVersion: string;
  readonly sha256: string;
  readonly method: string;
  readonly license: string;
  readonly network: string;
  readonly runtime: string;
  readonly trainedFromCorpusDigest: string;
};

export const HELP_MODEL_PROVENANCE = Object.freeze(HELP_MODEL_PROVENANCE_DATA) as unknown as HelpModelProvenance;

/** Decode base64 to bytes without a Node-only or browser-only API. */
function decodeBase64(value: string): Int8Array {
  const binary = atob(value);
  const bytes = new Int8Array(binary.length);
  for (let index = 0; index < binary.length; index += 1) {
    // charCodeAt yields 0..255; reinterpret the high half as negative int8.
    const code = binary.charCodeAt(index);
    bytes[index] = code > 127 ? code - 256 : code;
  }
  return bytes;
}

/**
 * Dequantize one int8 row into a unit vector.
 *
 * Renormalizing after dequantization removes the quantization scale from the
 * cosine entirely, so only the direction the model learned survives.
 */
function decodeRows(base64: string, scales: readonly number[], rows: number, dims: number): Float64Array[] {
  const bytes = decodeBase64(base64);
  const vectors: Float64Array[] = [];
  for (let row = 0; row < rows; row += 1) {
    const vector = new Float64Array(dims);
    const scale = scales[row] ?? 0;
    let sum = 0;
    for (let index = 0; index < dims; index += 1) {
      const value = (bytes[row * dims + index] / 127) * scale;
      vector[index] = value;
      sum += value * value;
    }
    if (sum > 0) {
      const inverse = 1 / Math.sqrt(sum);
      for (let index = 0; index < dims; index += 1) vector[index] *= inverse;
    }
    vectors.push(vector);
  }
  return vectors;
}

const raw = HELP_EMBEDDING_MODEL as unknown as {
  schema: string;
  modelId: string;
  dims: number;
  corpusDigest: string;
  vocabulary: string[];
  idf: number[];
  termScales: number[];
  termVectors: string;
  subwords: string[];
  subwordScales: number[];
  subwordVectors: string;
  chunkIds: string[];
  chunkScales: number[];
  chunkVectors: string;
};

if (raw.schema !== "grokptah.help-embedding-model.v1") {
  throw new Error(`help model: unexpected schema ${raw.schema}`);
}
if (raw.corpusDigest !== HELP_CORPUS_DIGEST) {
  throw new Error(
    "help model: artifact was trained from a different corpus " +
      `(model ${raw.corpusDigest} != corpus ${HELP_CORPUS_DIGEST}); rebuild with scripts/build-help-model.mjs`,
  );
}
if (HELP_MODEL_PROVENANCE.trainedFromCorpusDigest !== HELP_CORPUS_DIGEST) {
  throw new Error("help model: provenance does not match the shipped corpus digest");
}

export const HELP_MODEL_DIMS = raw.dims;
export const HELP_MODEL_ID = raw.modelId;

const TERM_VECTORS = decodeRows(raw.termVectors, raw.termScales, raw.vocabulary.length, raw.dims);
const SUBWORD_VECTORS = decodeRows(raw.subwordVectors, raw.subwordScales, raw.subwords.length, raw.dims);
const CHUNK_VECTORS = decodeRows(raw.chunkVectors, raw.chunkScales, raw.chunkIds.length, raw.dims);

const TERM_INDEX = new Map(raw.vocabulary.map((term, index) => [term, index]));
const SUBWORD_INDEX = new Map(raw.subwords.map((gram, index) => [gram, index]));
const CHUNK_INDEX = new Map(raw.chunkIds.map((chunkId, index) => [chunkId, index]));

/** IDF for an unseen term: treat it as at least as rare as the rarest known one. */
const MAX_IDF = raw.idf.reduce((peak, value) => (value > peak ? value : peak), 0);

export function helpTermIdf(term: string): number {
  const index = TERM_INDEX.get(term);
  return index === undefined ? MAX_IDF : raw.idf[index]!;
}

export function helpChunkVector(chunkId: string): Float64Array | undefined {
  const index = CHUNK_INDEX.get(chunkId);
  return index === undefined ? undefined : CHUNK_VECTORS[index];
}

/**
 * Vector for a single term.
 *
 * In-vocabulary terms use their learned vector. Everything else is rebuilt
 * from its character trigrams, which is the path that makes misspellings and
 * unseen inflections retrievable instead of silently scoring zero.
 */
export function helpTermVector(term: string): Float64Array | undefined {
  const index = TERM_INDEX.get(term);
  if (index !== undefined) return TERM_VECTORS[index];
  const vector = new Float64Array(raw.dims);
  let matched = 0;
  for (const gram of charNgrams(term)) {
    const gramIndex = SUBWORD_INDEX.get(gram);
    if (gramIndex === undefined) continue;
    const gramVector = SUBWORD_VECTORS[gramIndex]!;
    for (let dim = 0; dim < raw.dims; dim += 1) vector[dim] += gramVector[dim]!;
    matched += 1;
  }
  if (matched === 0) return undefined;
  let sum = 0;
  for (let dim = 0; dim < raw.dims; dim += 1) sum += vector[dim]! * vector[dim]!;
  if (sum === 0) return undefined;
  const inverse = 1 / Math.sqrt(sum);
  for (let dim = 0; dim < raw.dims; dim += 1) vector[dim] *= inverse;
  return vector;
}

/**
 * Fold a token list into the model's space.
 *
 * This is the *same* arithmetic the builder used to embed each chunk, so both
 * sides of every cosine are produced identically. Uses only multiply, add,
 * divide, and sqrt, all correctly rounded, so scores are engine-stable.
 */
export function embedHelpTokens(tokens: readonly string[]): Float64Array | undefined {
  if (tokens.length === 0) return undefined;
  const frequencies = new Map<string, number>();
  for (const token of tokens) frequencies.set(token, (frequencies.get(token) ?? 0) + 1);
  const vector = new Float64Array(raw.dims);
  let contributed = false;
  for (const [term, frequency] of frequencies) {
    const termVector = helpTermVector(term);
    if (!termVector) continue;
    const weight = Math.sqrt(frequency) * helpTermIdf(term);
    for (let dim = 0; dim < raw.dims; dim += 1) vector[dim] += termVector[dim]! * weight;
    contributed = true;
  }
  if (!contributed) return undefined;
  let sum = 0;
  for (let dim = 0; dim < raw.dims; dim += 1) sum += vector[dim]! * vector[dim]!;
  if (sum === 0) return undefined;
  const inverse = 1 / Math.sqrt(sum);
  for (let dim = 0; dim < raw.dims; dim += 1) vector[dim] *= inverse;
  return vector;
}

const FAMILIARITY_CACHE = new Map<string, number>();

/** Character-trigram posting list over the vocabulary, for the OOV path. */
const GRAM_TO_TERMS = new Map<string, number[]>();
raw.vocabulary.forEach((term, index) => {
  for (const gram of new Set(charNgrams(term))) {
    const bucket = GRAM_TO_TERMS.get(gram);
    if (bucket) bucket.push(index);
    else GRAM_TO_TERMS.set(gram, [index]);
  }
});

const MAX_EDIT_DISTANCE = 3;

/** Levenshtein distance, abandoned once it provably exceeds `cutoff`. */
function boundedEditDistance(left: string, right: string, cutoff: number): number {
  if (Math.abs(left.length - right.length) > cutoff) return cutoff + 1;
  let previous = new Int32Array(right.length + 1);
  let current = new Int32Array(right.length + 1);
  for (let j = 0; j <= right.length; j += 1) previous[j] = j;
  for (let i = 1; i <= left.length; i += 1) {
    current[0] = i;
    let rowMinimum = current[0]!;
    for (let j = 1; j <= right.length; j += 1) {
      const substitution = previous[j - 1]! + (left[i - 1] === right[j - 1] ? 0 : 1);
      const deletion = previous[j]! + 1;
      const insertion = current[j - 1]! + 1;
      const best = substitution < deletion ? substitution : deletion;
      current[j] = best < insertion ? best : insertion;
      if (current[j]! < rowMinimum) rowMinimum = current[j]!;
    }
    if (rowMinimum > cutoff) return cutoff + 1;
    const swap = previous;
    previous = current;
    current = swap;
  }
  return previous[right.length]!;
}

/**
 * How well the model actually knows a term, in [0, 1].
 *
 * 1 for in-vocabulary terms. Otherwise `1 - editDistance / length` against the
 * closest vocabulary term, with candidates narrowed by shared trigrams so the
 * comparison stays cheap.
 *
 * This separates a misspelling from a word the corpus has never seen:
 * `chekpoint`/`checkpoint` and `quata`/`quota` are one edit apart, while
 * `sourdough` and `photosynthesis` are far from everything. Two cheaper
 * measures were tried and rejected — cosine to the nearest embedding
 * neighbour does not separate them at all (a subword vector is an average of
 * many term vectors and sits close to everything), and raw trigram Dice
 * punishes short words too hard, scoring `quata` no better than `sourdough`.
 *
 * Without this gate the subword backoff hands every unknown term a
 * plausible-looking vector, and unanswerable questions score high enough to be
 * answered instead of abstained on.
 */
export function helpTermFamiliarity(term: string): number {
  const cached = FAMILIARITY_CACHE.get(term);
  if (cached !== undefined) return cached;
  let familiarity = 0;
  if (TERM_INDEX.has(term)) {
    familiarity = 1;
  } else {
    const candidates = new Set<number>();
    for (const gram of new Set(charNgrams(term))) {
      for (const index of GRAM_TO_TERMS.get(gram) ?? []) candidates.add(index);
    }
    for (const index of candidates) {
      const candidate = raw.vocabulary[index]!;
      const distance = boundedEditDistance(term, candidate, MAX_EDIT_DISTANCE);
      if (distance > MAX_EDIT_DISTANCE) continue;
      const longest = term.length > candidate.length ? term.length : candidate.length;
      const similarity = 1 - distance / longest;
      if (similarity > familiarity) familiarity = similarity;
    }
  }
  FAMILIARITY_CACHE.set(term, familiarity);
  return familiarity;
}

/**
 * Share of the query the model can actually account for, weighted by term
 * importance. Feeds the semantic component so an unanswerable question cannot
 * borrow confidence from vectors built out of unknown words.
 */
export function helpQueryFamiliarity(tokens: readonly string[]): number {
  if (tokens.length === 0) return 0;
  let weighted = 0;
  let total = 0;
  for (const token of tokens) {
    const weight = helpTermIdf(token);
    const familiarity = helpTermFamiliarity(token);
    // Below ~0.6 similarity a token is not a plausible misspelling of anything
    // in the corpus, so it contributes no confidence at all.
    const shaped = familiarity >= 1 ? 1 : Math.max(0, (familiarity - 0.6) / 0.3);
    weighted += weight * (shaped > 1 ? 1 : shaped);
    total += weight;
  }
  return total === 0 ? 0 : weighted / total;
}

/** Cosine similarity of two unit vectors. */
export function cosineSimilarity(left: Float64Array, right: Float64Array): number {
  let sum = 0;
  for (let dim = 0; dim < left.length; dim += 1) sum += left[dim]! * right[dim]!;
  return sum;
}

/**
 * Recompute the artifact checksum and compare it to the provenance record.
 *
 * Hashing ~200 KB is too costly to run on every import, so the binding checked
 * at load is the corpus digest; this full check runs in tests and in
 * `scripts/verify-help-model.mjs`.
 */
export function verifyHelpModelChecksum(): { ok: boolean; expected: string; actual: string } {
  const actual = `sha256:${sha256Hex(canonicalJson(raw))}`;
  return { ok: actual === HELP_MODEL_PROVENANCE.sha256, expected: HELP_MODEL_PROVENANCE.sha256, actual };
}

export const HELP_MODEL_STATS = Object.freeze({
  modelId: raw.modelId,
  dims: raw.dims,
  vocabularySize: raw.vocabulary.length,
  subwordCount: raw.subwords.length,
  chunkCount: raw.chunkIds.length,
  corpusDigest: raw.corpusDigest,
});
