/**
 * Shared text normalization for Help retrieval.
 *
 * The model builder and the runtime retriever both import this module, so a
 * checked-in embedding artifact can never drift from the tokenizer that
 * queries it. Every function here is pure and allocation-bounded.
 */

/** Hard bounds so a hostile query cannot drive unbounded work. */
export const HELP_QUERY_MAX_CHARS = 512;
export const HELP_QUERY_MAX_TERMS = 64;
export const HELP_NGRAM_SIZE = 3;

/**
 * Stop words for the corpus locales (en/es/fr/de).
 *
 * Deliberately small: dropping a content word hurts recall far more than
 * keeping a function word costs precision, and BM25 already discounts
 * high-frequency terms.
 */
const STOP_WORDS = new Set([
  // English
  "a", "an", "and", "are", "as", "at", "be", "by", "can", "do", "does", "for",
  "from", "has", "have", "how", "i", "if", "in", "is", "it", "its", "me", "my",
  "no", "not", "of", "on", "or", "so", "that", "the", "then", "this", "to",
  "was", "we", "what", "when", "where", "which", "why", "will", "with", "you", "your",
  // Spanish
  "de", "la", "el", "los", "las", "un", "una", "que", "por", "para", "con",
  "como", "se", "es", "y", "o", "mi", "al", "lo", "sin",
  // French
  "le", "les", "des", "du", "une", "est", "et", "ou", "pour", "dans", "sur",
  "que", "qui", "ce", "je", "il", "elle", "au", "aux", "sans",
  // German
  "der", "die", "das", "den", "dem", "ein", "eine", "einen", "und", "oder",
  "fur", "mit", "von", "im", "ist", "sie", "wie", "was", "auf", "zu",
]);

/**
 * Lowercase, decompose, and strip combining marks.
 *
 * Diacritic folding is what lets `sesion` match `sesión` and `fur` match
 * `für`, which matters for both misspellings and the localized articles.
 */
export function normalizeText(value: string): string {
  return value
    .normalize("NFKD")
    .replace(/\p{M}+/gu, "")
    .toLowerCase();
}

/**
 * Conservative suffix stripping.
 *
 * Only endings that are unambiguous across the corpus vocabulary are removed;
 * aggressive stemming collapses distinct product terms (`lease`/`leases` is
 * safe, `approval`/`approve` is not, and is left alone).
 */
export function stem(term: string): string {
  if (term.length > 4 && term.endsWith("ies")) return `${term.slice(0, -3)}y`;
  if (term.length > 4 && term.endsWith("sses")) return term.slice(0, -2);
  if (term.length > 4 && term.endsWith("ing")) {
    const base = term.slice(0, -3);
    return base.length > 2 ? base : term;
  }
  if (term.length > 4 && term.endsWith("ed")) {
    const base = term.slice(0, -2);
    return base.length > 2 ? base : term;
  }
  if (
    term.length > 3 &&
    term.endsWith("s") &&
    !term.endsWith("ss") &&
    !term.endsWith("us") &&
    !term.endsWith("is")
  ) {
    return term.slice(0, -1);
  }
  return term;
}

/** Split normalized text into raw (unstemmed) tokens. */
export function rawTokens(value: string): string[] {
  return normalizeText(value)
    .split(/[^\p{L}\p{N}]+/u)
    .filter((token) => token.length > 0);
}

/**
 * Content tokens: normalized, stop-worded, stemmed, and count-bounded.
 * Single characters are dropped; digits are kept because version and error
 * identifiers matter for exact lookup.
 */
export function tokenize(value: string, maxTerms = HELP_QUERY_MAX_TERMS): string[] {
  const tokens: string[] = [];
  for (const token of rawTokens(value)) {
    if (token.length < 2) continue;
    if (STOP_WORDS.has(token)) continue;
    tokens.push(stem(token));
    if (tokens.length >= maxTerms) break;
  }
  return tokens;
}

/**
 * Character n-grams with boundary markers.
 *
 * These back the out-of-vocabulary path: a misspelled or unseen term still
 * gets a vector by averaging the n-grams it shares with known vocabulary,
 * which is what makes `chekpoint` land near `checkpoint`.
 */
export function charNgrams(term: string, size = HELP_NGRAM_SIZE): string[] {
  const padded = `^${term}$`;
  if (padded.length <= size) return [padded];
  const grams: string[] = [];
  for (let index = 0; index + size <= padded.length; index += 1) {
    grams.push(padded.slice(index, index + size));
  }
  return grams;
}

/** Clamp a caller-supplied query to the bounded form retrieval accepts. */
export function boundQuery(query: string): string {
  return query.slice(0, HELP_QUERY_MAX_CHARS).trim();
}

/** True when the token is a stop word after normalization. */
export function isStopWord(token: string): boolean {
  return STOP_WORDS.has(normalizeText(token));
}
