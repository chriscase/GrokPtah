/**
 * Text normalisation shared by every retrieval signal.
 *
 * Normalisation has to be one function. If the index and the query normalise
 * differently — even by a stripped accent or a dropped plural — a term that
 * exists in the corpus becomes unfindable, and the failure looks like missing
 * content rather than a bug.
 */

const STOP_WORDS = new Set([
  "a", "an", "and", "are", "as", "at", "be", "by", "can", "do", "does", "for",
  "from", "has", "have", "how", "i", "if", "in", "is", "it", "its", "me", "my",
  "no", "not", "of", "on", "or", "so", "that", "the", "then", "this", "to",
  "was", "what", "when", "where", "which", "why", "will", "with", "you", "your",
]);

/**
 * Fold one token to its comparison form: accents removed, lowercased, and a
 * light plural stripped. Deliberately not a real stemmer — an aggressive one
 * collapses distinct identifiers, and identifiers are exactly what a reader
 * searching Help types.
 */
export function canonicalTerm(value: string): string {
  const normalized = value.normalize("NFKD").replace(/\p{M}/gu, "").toLocaleLowerCase();
  if (normalized.length > 4 && normalized.endsWith("ies")) {
    return `${normalized.slice(0, -3)}y`;
  }
  if (
    normalized.length > 4 &&
    normalized.endsWith("s") &&
    !normalized.endsWith("ss") &&
    !normalized.endsWith("us")
  ) {
    return normalized.slice(0, -1);
  }
  return normalized;
}

/** Split text into comparable terms, dropping stop words and single letters. */
export function terms(value: string): string[] {
  return value
    .split(/[^\p{L}\p{N}]+/u)
    .map(canonicalTerm)
    .filter((term) => term.length > 1 && !STOP_WORDS.has(term));
}

/** Terms kept in order, including stop words — used for phrase proximity. */
export function orderedTerms(value: string): string[] {
  return value
    .split(/[^\p{L}\p{N}]+/u)
    .map(canonicalTerm)
    .filter((term) => term.length > 0);
}

/**
 * Character trigrams over the normalised string.
 *
 * These carry the "means roughly the same" signal without a model file. A
 * pinned neural embedding would need a weights artifact whose provenance and
 * licence this repository would then have to qualify; trigram overlap is
 * weaker but it is deterministic, byte-stable across platforms, and honest
 * about being lexical. It is what makes "restart duplicate" find
 * "recover a durable run" while a pure term index does not.
 */
export function trigrams(value: string): string[] {
  const normalized = ` ${value.normalize("NFKD").replace(/\p{M}/gu, "").toLocaleLowerCase().replace(/[^\p{L}\p{N}]+/gu, " ").trim()} `;
  const out: string[] = [];
  for (let index = 0; index + 3 <= normalized.length; index += 1) {
    out.push(normalized.slice(index, index + 3));
  }
  return out;
}

/** Sparse vector of term counts, L2-normalised. */
export function vectorize(tokens: readonly string[]): Map<string, number> {
  const counts = new Map<string, number>();
  for (const token of tokens) {
    counts.set(token, (counts.get(token) ?? 0) + 1);
  }
  let norm = 0;
  for (const count of counts.values()) norm += count * count;
  norm = Math.sqrt(norm) || 1;
  for (const [token, count] of counts) counts.set(token, count / norm);
  return counts;
}

/** Cosine similarity of two L2-normalised sparse vectors. */
export function cosine(left: Map<string, number>, right: Map<string, number>): number {
  const [small, large] = left.size <= right.size ? [left, right] : [right, left];
  let total = 0;
  for (const [token, weight] of small) {
    const other = large.get(token);
    if (other !== undefined) total += weight * other;
  }
  return total;
}
