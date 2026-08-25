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

const CORRECTION_CACHE = new Map<string, string | null>();

/** Character-trigram posting list over the vocabulary, for the OOV path. */
const GRAM_TO_TERMS = new Map<string, number[]>();
raw.vocabulary.forEach((term, index) => {
  for (const gram of new Set(charNgrams(term))) {
    const bucket = GRAM_TO_TERMS.get(gram);
    if (bucket) bucket.push(index);
    else GRAM_TO_TERMS.set(gram, [index]);
  }
});

/**
 * Damerau-Levenshtein distance, abandoned once it provably exceeds `cutoff`.
 *
 * Adjacent transposition counts as one edit, not two. That single difference
 * is what lets `reveiw`, `reciept`, `durabel`, `promotoin`, and `discrad` be
 * recognized as one-edit typos of real corpus words.
 */
function boundedEditDistance(left: string, right: string, cutoff: number): number {
  if (Math.abs(left.length - right.length) > cutoff) return cutoff + 1;
  const width = right.length + 1;
  let twoBack = new Int32Array(width);
  let previous = new Int32Array(width);
  let current = new Int32Array(width);
  for (let j = 0; j < width; j += 1) previous[j] = j;
  for (let i = 1; i <= left.length; i += 1) {
    current[0] = i;
    let rowMinimum = current[0]!;
    for (let j = 1; j < width; j += 1) {
      const cost = left[i - 1] === right[j - 1] ? 0 : 1;
      const substitution = previous[j - 1]! + cost;
      const deletion = previous[j]! + 1;
      const insertion = current[j - 1]! + 1;
      let best = substitution < deletion ? substitution : deletion;
      if (insertion < best) best = insertion;
      if (i > 1 && j > 1 && left[i - 1] === right[j - 2] && left[i - 2] === right[j - 1]) {
        const transposition = twoBack[j - 2]! + 1;
        if (transposition < best) best = transposition;
      }
      current[j] = best;
      if (best < rowMinimum) rowMinimum = best;
    }
    if (rowMinimum > cutoff) return cutoff + 1;
    const recycled = twoBack;
    twoBack = previous;
    previous = current;
    current = recycled;
  }
  return previous[right.length]!;
}

/**
 * Edit budget by term length.
 *
 * Short words are not corrected at all: within one edit a four-letter word
 * resembles far too much of any vocabulary to guess at. This is what keeps
 * `bake` and `bread` from being "corrected" into corpus terms and making an
 * unanswerable question look answerable.
 */
function editBudget(length: number): number {
  if (length >= 9) return 2;
  if (length >= 5) return 1;
  return 0;
}

/**
 * Length of the common prefix of two terms.
 *
 * A correction must agree with the original near the start. Typos rarely
 * change a word's opening, and without this guard the vocabulary swallows
 * ordinary English: `please` becomes `lease` and `contents` becomes
 * `consent`, handing off-topic queries lexical evidence they should not have.
 */
function commonPrefixLength(left: string, right: string): number {
  const limit = left.length < right.length ? left.length : right.length;
  let index = 0;
  while (index < limit && left[index] === right[index]) index += 1;
  return index;
}

const MIN_CORRECTION_PREFIX = 2;

/**
 * Map a query term onto the vocabulary, correcting a likely misspelling.
 *
 * Returns the term itself when it is already known, the corrected term when
 * one is unambiguously close, and null when the corpus has no such word.
 *
 * Correcting the query is what makes misspellings work: BM25, coordination,
 * and the embedding fold-in then all operate on real vocabulary instead of
 * each separately trying to cope with a typo. Ties at the same edit distance
 * resolve to the lexicographically smallest candidate so the correction is
 * reproducible.
 */
export function helpCorrectTerm(term: string): string | null {
  const cached = CORRECTION_CACHE.get(term);
  if (cached !== undefined) return cached;
  let corrected: string | null = null;
  if (TERM_INDEX.has(term)) {
    corrected = term;
  } else {
    const budget = editBudget(term.length);
    if (budget > 0) {
      const candidates = new Set<number>();
      for (const gram of new Set(charNgrams(term))) {
        for (const index of GRAM_TO_TERMS.get(gram) ?? []) candidates.add(index);
      }
      let bestDistance = budget + 1;
      for (const index of candidates) {
        const candidate = raw.vocabulary[index]!;
        if (commonPrefixLength(term, candidate) < MIN_CORRECTION_PREFIX) continue;
        const distance = boundedEditDistance(term, candidate, budget);
        if (distance > budget) continue;
        if (distance < bestDistance || (distance === bestDistance && corrected !== null && candidate < corrected)) {
          bestDistance = distance;
          corrected = candidate;
        }
      }
    }
  }
  CORRECTION_CACHE.set(term, corrected);
  return corrected;
}

export type HelpQueryTerm = {
  readonly original: string;
  /** Vocabulary term used for retrieval, or null when the corpus has none. */
  readonly term: string | null;
  readonly corrected: boolean;
};

/** Resolve every query token against the vocabulary, correcting misspellings. */
export function resolveHelpQueryTerms(tokens: readonly string[]): HelpQueryTerm[] {
  return tokens.map((original) => {
    const term = helpCorrectTerm(original);
    return { original, term, corrected: term !== null && term !== original };
  });
}

/**
 * Share of the query the corpus can account for, weighted by term importance.
 *
 * Scales the semantic component so a question about something the corpus never
 * covers cannot borrow confidence from vectors assembled out of unknown words.
 */
export function helpQueryFamiliarity(resolved: readonly HelpQueryTerm[]): number {
  if (resolved.length === 0) return 0;
  let known = 0;
  let total = 0;
  for (const entry of resolved) {
    const weight = helpTermIdf(entry.term ?? entry.original);
    total += weight;
    if (entry.term !== null) known += weight;
  }
  return total === 0 ? 0 : known / total;
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
