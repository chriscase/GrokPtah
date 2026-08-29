/**
 * Offline hybrid retrieval over the canonical corpus.
 *
 * Two signals are fused because each fails where the other works:
 *
 * * **BM25 over terms** is exact. It finds `sessionNewKind`, `cursor_expired`,
 *   or `Grok Build` — the identifiers a reader types when they already know
 *   the vocabulary. It finds nothing when they do not.
 * * **Trigram cosine** is approximate. It connects "restart duplicate" to
 *   "recover a durable run" without either sharing a term. It also happily
 *   ranks something plausible for a question the corpus cannot answer.
 *
 * Fusing them keeps exactness where it exists and reach where it does not, and
 * the abstention threshold below is what stops the approximate half from
 * confidently answering an unanswerable question.
 *
 * Everything here runs in the consumer's own process against bytes it already
 * has. There is no network call in this module and no way to add one: offline
 * retrieval is the product, not a degraded mode for when a provider is down.
 */

import type { HelpChunk, HelpCorpus, HelpTopic } from "../generated/contract";
import { HELP_CORPUS } from "../canonical/corpus";
import { cosine, terms, trigrams, vectorize } from "./text";

/** Weights on the fused score. Tuned against the gold set in `eval/goldset.ts`. */
export const HELP_FUSION_WEIGHTS = Object.freeze({ lexical: 0.62, semantic: 0.38 });

/**
 * Below this fused score the retriever abstains rather than answering.
 *
 * An unanswerable question with a confident-looking result is worse than no
 * result: the reader stops looking, having been told something that was never
 * checked.
 *
 * Calibrated against `helpRetrieval.test.ts`: the worst gold-set positive
 * scores about 0.66 and the best negative about 0.18, so this sits in the gap
 * with room on both sides. It is only meaningful because the lexical score is
 * absolute — under the previous peak-relative normalisation the top hit always
 * scored 1.0 and no threshold could have separated anything.
 */
export const HELP_ABSTENTION_THRESHOLD = 0.4;

export const HELP_RETRIEVAL_DEFAULT_LIMIT = 8;
export const HELP_RETRIEVAL_MAX_LIMIT = 25;

/** BM25 saturation and length-normalisation constants. */
const BM25_K1 = 1.2;
const BM25_B = 0.65;

/** Field weights. A title match means more than a body mention. */
const FIELD_WEIGHT: Record<HelpChunk["kind"], number> = { title: 2.4, summary: 1.5, body: 1 };

/**
 * Weight on an article's aliases and keywords.
 *
 * Aliases are the phrasings a reader actually types — "clicking", "mouse and
 * keyboard", "restart duplicate" — and they are deliberately *not* chunks. A
 * chunk is quotable: a citation span points into one, so anything that becomes
 * a chunk becomes something an answer can quote as if it were documentation.
 * Aliases are search hints written for retrieval, not prose anyone should be
 * shown as a source, so they steer ranking here and can never be cited.
 */
const HINT_WEIGHT = 0.9;

/**
 * Saturation constant for the lexical score.
 *
 * The lexical half used to be divided by the best score in the result set,
 * which meant the top hit always normalised to exactly 1.0 — for a perfect
 * match and for a single incidental term alike. The abstention threshold was
 * then comparing against a number that carried no information about quality,
 * and it only appeared to work because unanswerable questions happened to
 * match nothing at all. `score / (score + K)` is absolute instead: bounded in
 * [0, 1), monotonic, and a weak match stays weak no matter what it is next to.
 */
const LEXICAL_SATURATION = 4;

export type HelpRetrievalMode = "offline-hybrid";

export type HelpScoreComponents = {
  readonly lexical: number;
  readonly semantic: number;
  readonly fused: number;
};

export type HelpRetrievalResult = {
  readonly articleId: string;
  readonly chunkId: string;
  readonly title: string;
  readonly topic: HelpTopic;
  readonly summary: string;
  /** The exact chunk bytes that matched. */
  readonly text: string;
  readonly sourceIds: readonly string[];
  readonly score: HelpScoreComponents;
  readonly matchedTerms: readonly string[];
};

/**
 * Why retrieval declined to return results.
 *
 * `no-match` and `below-threshold` are kept apart because a surface must say
 * different things about them: nothing in the corpus mentioned the question at
 * all, versus something did and none of it was good enough to lead. Collapsing
 * them would make an honest "we do not document this" indistinguishable from a
 * weak match a reader could still usefully skim.
 */
export type HelpAbstentionReason =
  | "no-query"
  | "no-match"
  | "below-threshold"
  | "empty-corpus";

export type HelpRetrievalOutcome =
  | { readonly kind: "results"; readonly mode: HelpRetrievalMode; readonly corpusDigest: string; readonly results: readonly HelpRetrievalResult[] }
  | { readonly kind: "abstained"; readonly mode: HelpRetrievalMode; readonly corpusDigest: string; readonly reason: HelpAbstentionReason };

export type HelpRetrievalOptions = {
  readonly limit?: number;
  readonly topic?: HelpTopic | "all";
  readonly corpus?: HelpCorpus;
};

type IndexEntry = {
  readonly chunk: HelpChunk;
  readonly terms: readonly string[];
  readonly termSet: ReadonlySet<string>;
  readonly trigrams: Map<string, number>;
  readonly length: number;
  /** Terms from the parent article's aliases and keywords. Never citable. */
  readonly hintSet: ReadonlySet<string>;
  readonly hintTerms: readonly string[];
  readonly hintTrigrams: Map<string, number>;
};

type Index = {
  readonly digest: string;
  readonly entries: readonly IndexEntry[];
  readonly documentFrequency: ReadonlyMap<string, number>;
  readonly averageLength: number;
};

/**
 * Built indexes, keyed by the corpus object itself.
 *
 * Keying on `corpus.digest` looked equivalent and was not: a caller that
 * builds a corpus — a host-filtered view, a test fixture, an embedder's own
 * bundle — chooses its own digest, and two different documents carrying the
 * same string were served one another's index. The shipped corpora derive
 * their digests from content and were never at risk, which is exactly what
 * made the hazard easy to miss.
 *
 * Object identity cannot collide, and a `WeakMap` lets a corpus that goes out
 * of scope take its index with it. The shipped corpus is a module constant, so
 * the hot path still builds once.
 */
const INDEX_CACHE = new WeakMap<HelpCorpus, Index>();

function buildIndex(corpus: HelpCorpus): Index {
  const hintsByArticle = new Map<string, string>(
    corpus.articles.map((article) => [
      article.id,
      [...article.aliases, ...article.keywords].join(" "),
    ]),
  );
  const entries: IndexEntry[] = corpus.chunks.map((chunk) => {
    const chunkTerms = terms(chunk.text);
    const hints = hintsByArticle.get(chunk.article_id) ?? "";
    return {
      chunk,
      terms: chunkTerms,
      termSet: new Set(chunkTerms),
      trigrams: vectorize(trigrams(chunk.text)),
      length: chunkTerms.length,
      hintSet: new Set(terms(hints)),
      hintTerms: terms(hints),
      hintTrigrams: vectorize(trigrams(hints)),
    };
  });
  const documentFrequency = new Map<string, number>();
  for (const entry of entries) {
    for (const term of entry.termSet) {
      documentFrequency.set(term, (documentFrequency.get(term) ?? 0) + 1);
    }
  }
  const averageLength =
    entries.reduce((total, entry) => total + entry.length, 0) / Math.max(1, entries.length);
  return { digest: corpus.digest, entries, documentFrequency, averageLength };
}

function indexFor(corpus: HelpCorpus): Index {
  const cached = INDEX_CACHE.get(corpus);
  if (cached) return cached;
  const built = buildIndex(corpus);
  INDEX_CACHE.set(corpus, built);
  return built;
}

/**
 * How well the query matches an article's aliases and keywords.
 *
 * Exact matches count fully; a shared prefix of at least four characters
 * counts half. Readers type stems — "click" for the alias "clicking", "safe"
 * for "safely" — and an exact-only comparison misses precisely the phrasings
 * the aliases were written to catch. Four characters is short enough to catch
 * real stems and long enough that "run" does not match "runtime".
 */
function hintScore(queryTerms: readonly string[], entry: IndexEntry): number {
  let total = 0;
  for (const term of queryTerms) {
    if (entry.hintSet.has(term)) {
      total += 1;
      continue;
    }
    const prefixed = entry.hintTerms.some(
      (hint) =>
        term.length >= 4 &&
        hint.length >= 4 &&
        (hint.startsWith(term) || term.startsWith(hint)),
    );
    if (prefixed) total += 0.5;
  }
  return total;
}

function bm25(queryTerms: readonly string[], entry: IndexEntry, index: Index): number {
  const documentCount = index.entries.length || 1;
  let score = 0;
  for (const term of queryTerms) {
    const frequency = entry.terms.filter((candidate) => candidate === term).length;
    if (frequency === 0) continue;
    const documentFrequency = index.documentFrequency.get(term) ?? 0;
    const idf = Math.log(
      1 + (documentCount - documentFrequency + 0.5) / (documentFrequency + 0.5),
    );
    const denominator =
      frequency + BM25_K1 * (1 - BM25_B + (BM25_B * entry.length) / (index.averageLength || 1));
    score += idf * ((frequency * (BM25_K1 + 1)) / denominator);
  }
  return score * FIELD_WEIGHT[entry.chunk.kind];
}

/**
 * Search the corpus offline.
 *
 * Returns an explicit abstention rather than an empty list, because "we found
 * nothing" and "we are not confident enough to say" are different answers and
 * a caller should be able to render them differently.
 */
export function searchHelpCorpus(
  query: string,
  options: HelpRetrievalOptions = {},
): HelpRetrievalOutcome {
  const corpus = options.corpus ?? HELP_CORPUS;
  const mode: HelpRetrievalMode = "offline-hybrid";
  if (corpus.chunks.length === 0) {
    return { kind: "abstained", mode, corpusDigest: corpus.digest, reason: "empty-corpus" };
  }
  const queryTerms = terms(query);
  const queryTrigrams = vectorize(trigrams(query));
  if (queryTerms.length === 0 && queryTrigrams.size === 0) {
    return { kind: "abstained", mode, corpusDigest: corpus.digest, reason: "no-query" };
  }

  const index = indexFor(corpus);
  const limit = Math.max(1, Math.min(options.limit ?? HELP_RETRIEVAL_DEFAULT_LIMIT, HELP_RETRIEVAL_MAX_LIMIT));

  const scored: HelpRetrievalResult[] = [];
  const raw: Array<{ entry: IndexEntry; lexical: number; semantic: number }> = [];

  for (const entry of index.entries) {
    const article = corpus.articles.find((candidate) => candidate.id === entry.chunk.article_id);
    if (!article) continue;
    if (options.topic && options.topic !== "all" && article.topic !== options.topic) continue;
    const hintHits = hintScore(queryTerms, entry);
    const lexical =
      bm25(queryTerms, entry, index) + HINT_WEIGHT * hintHits * FIELD_WEIGHT[entry.chunk.kind];
    const semantic = Math.min(
      1,
      Math.max(
        cosine(queryTrigrams, entry.trigrams),
        HINT_WEIGHT * cosine(queryTrigrams, entry.hintTrigrams),
      ) * FIELD_WEIGHT[entry.chunk.kind],
    );
    if (lexical === 0 && semantic === 0) continue;
    raw.push({ entry, lexical, semantic });
  }

  for (const { entry, lexical, semantic } of raw) {
    const normalizedLexical = lexical / (lexical + LEXICAL_SATURATION);
    const fused =
      HELP_FUSION_WEIGHTS.lexical * normalizedLexical + HELP_FUSION_WEIGHTS.semantic * semantic;
    const article = corpus.articles.find((candidate) => candidate.id === entry.chunk.article_id);
    if (!article) continue;
    scored.push({
      articleId: article.id,
      chunkId: entry.chunk.id,
      title: article.title,
      topic: article.topic,
      summary: article.summary,
      text: entry.chunk.text,
      sourceIds: entry.chunk.source_ids,
      score: { lexical: normalizedLexical, semantic, fused },
      matchedTerms: queryTerms.filter(
        (term) => entry.termSet.has(term) || entry.hintSet.has(term),
      ),
    });
  }

  // Ties break on code point, not `localeCompare`. Collation ignores
  // punctuation at primary strength, so under ICU an id like `help-a` can sort
  // either side of `helpa` depending on the host locale — a ranking that moves
  // with the reader's machine is not the deterministic one this contract
  // promises.
  const byCodePoint = (left: string, right: string): number =>
    left < right ? -1 : left > right ? 1 : 0;
  scored.sort(
    (left, right) =>
      right.score.fused - left.score.fused ||
      byCodePoint(left.articleId, right.articleId) ||
      byCodePoint(left.chunkId, right.chunkId),
  );

  const best = scored[0];
  if (!best) {
    // Nothing scored at all: the corpus does not mention this question.
    return { kind: "abstained", mode, corpusDigest: corpus.digest, reason: "no-match" };
  }
  if (best.score.fused < HELP_ABSTENTION_THRESHOLD) {
    return { kind: "abstained", mode, corpusDigest: corpus.digest, reason: "below-threshold" };
  }

  // Collapse to the best chunk per article so one long article cannot fill the
  // page and hide every other answer.
  const seen = new Set<string>();
  const results: HelpRetrievalResult[] = [];
  for (const result of scored) {
    if (seen.has(result.articleId)) continue;
    seen.add(result.articleId);
    results.push(result);
    if (results.length >= limit) break;
  }

  return { kind: "results", mode, corpusDigest: corpus.digest, results };
}
