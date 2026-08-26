/**
 * Offline-first hybrid Help retrieval.
 *
 * Combines three independent, separately reportable signals:
 *   - BM25 over canonical chunks and article metadata (lexical);
 *   - exact whole-query phrase presence (keyword authority);
 *   - cosine similarity in the pinned local embedding space (semantic).
 *
 * Everything runs offline. No provider, no network, and no configuration is
 * required for search to be fully useful.
 */
import { HELP_CORPUS, HELP_CORPUS_DIGEST, getHelpArticle, getHelpChunk } from "../canonical/corpus";
import type { HelpAccess, HelpAudience, HelpSourceAnchor, HelpTopic } from "../canonical/types";
import {
  HELP_MODEL_ID,
  cosineSimilarity,
  embedHelpTokens,
  helpChunkVector,
  helpQueryFamiliarity,
  resolveHelpQueryTerms,
} from "../model/artifact";
import { buildHelpExcerpt, sanitizeHelpText, type HelpExcerpt } from "./highlight";
import { redactHelpText, type HelpRedaction } from "./redact";
import { scoreLexical, type LexicalDocument } from "./lexical";
import { HELP_QUERY_MAX_TERMS, boundQuery, normalizeText, tokenize } from "./text";

export const HELP_RETRIEVAL_SCHEMA = "grokptah.help-retrieval.v1" as const;

/** Result-count bounds. A caller cannot ask for an unbounded page. */
export const HELP_RETRIEVAL_DEFAULT_LIMIT = 8;
export const HELP_RETRIEVAL_MAX_LIMIT = 25;

/**
 * Fusion weights. They sum to 1 so the fused score is directly comparable
 * across queries, which is what makes a single abstention threshold meaningful.
 */
export const HELP_FUSION_WEIGHTS = Object.freeze({
  lexical: 0.67,
  semantic: 0.25,
  exactPhrase: 0.08,
});

/**
 * Coordination damping exponent.
 *
 * Applied as `coordination ** HELP_COORDINATION_EXPONENT`. Chosen by the same
 * sweep as the weights: a strong exponent suppresses single-rare-term false
 * matches but also punishes long natural-language paraphrases, most of whose
 * words no article contains. With query correction and a vocabulary trained on
 * the cited sources, mild damping plus the abstention threshold does better
 * than harsh coordination on both counts.
 */
export const HELP_COORDINATION_EXPONENT = 0.15;

/**
 * BM25 saturation constant.
 *
 * Normalizing by the per-query maximum would force the top hit to 1.0 for
 * every query, including nonsense ones, and destroy any calibrated
 * abstention. A saturating transform keeps absolute scores meaningful.
 */
const BM25_SATURATION = 6;

/**
 * Minimum fused score to answer rather than abstain.
 *
 * Calibrated by an offline grid sweep over fusion weights, coordination
 * damping, and threshold against the 146-query gold set, selecting the point
 * that maximizes Recall@1 subject to a false-answer rate at or below 5%.
 * Reproduce with `scripts/run-help-eval.mjs`; the operating point and the
 * neighbouring frontier are recorded in `docs/HELP_SEMANTIC_CORE.md`.
 *
 * The frontier is real: driving false answers to zero costs roughly five more
 * points of answerable coverage. This point declines to answer about one
 * answerable query in twelve rather than inventing support for one that has
 * none.
 */
export const HELP_ABSTENTION_THRESHOLD = 0.38;

export type HelpRetrievalMode = "hybrid" | "lexical" | "semantic";

export type HelpRetrievalOptions = {
  readonly limit?: number;
  readonly topic?: HelpTopic | "all";
  readonly audience?: HelpAudience;
  readonly access?: readonly HelpAccess[];
  /**
   * Explicit authority for non-public Help. Missing or empty means public-only,
   * even when `access` asks for gated/operator articles.
   */
  readonly authorizedCapabilities?: readonly string[];
  /** Restrict the signals used. Default `hybrid`. */
  readonly mode?: HelpRetrievalMode;
  /** Fail closed unless the corpus digest matches this value exactly. */
  readonly expectCorpusDigest?: string;
  /** Override the calibrated abstention threshold. */
  readonly minConfidence?: number;
  /** Cooperative cancellation. */
  readonly signal?: AbortSignal;
};

/** Every component that contributed to a score, for display and debugging. */
export type HelpScoreComponents = {
  readonly lexicalBm25: number;
  readonly lexicalNormalized: number;
  /** Share of distinct query terms this article matched, in [0, 1]. */
  readonly coordination: number;
  readonly exactPhrase: number;
  readonly semanticCosine: number;
  /** Semantic cosine after the query-familiarity discount. */
  readonly semanticEffective: number;
  readonly fused: number;
};

export type HelpCitation = {
  readonly sourceId: string;
  readonly path: string;
  readonly heading: string;
  readonly articleId: string;
  readonly chunkId: string;
};

export type HelpRetrievalResult = {
  readonly rank: number;
  readonly articleId: string;
  readonly chunkId: string;
  readonly title: string;
  readonly topic: HelpTopic;
  readonly summary: string;
  readonly locale: string;
  readonly access: HelpAccess;
  readonly score: number;
  readonly components: HelpScoreComponents;
  readonly matchedTerms: readonly string[];
  readonly excerpt: HelpExcerpt;
  readonly citations: readonly HelpCitation[];
  /** Plain-language account of why this result scored where it did. */
  readonly explanation: string;
};

export type HelpAbstentionReason =
  | "none"
  | "empty-query"
  | "no-match"
  | "below-confidence"
  | "cancelled";

export type HelpRetrievalOutcome = {
  readonly schema: typeof HELP_RETRIEVAL_SCHEMA;
  readonly corpusDigest: string;
  readonly corpusContentVersion: string;
  readonly modelId: string;
  readonly mode: HelpRetrievalMode;
  readonly query: string;
  readonly results: readonly HelpRetrievalResult[];
  readonly abstained: boolean;
  readonly abstentionReason: HelpAbstentionReason;
  /** Fused score of the best candidate, whether or not it was returned. */
  readonly confidence: number;
  /** Gap between the best and second-best article. */
  readonly margin: number;
  /** True when the query was clamped to the accepted bound. */
  readonly queryTruncated: boolean;
  /**
   * Share of the query the embedding model can account for, in [0, 1].
   * Low values mean the question is outside what the corpus covers.
   */
  readonly queryFamiliarity: number;
  /** Misspelled terms mapped onto the vocabulary, for "showing results for". */
  readonly corrections: readonly { readonly from: string; readonly to: string }[];
  /**
   * Credential-shaped or private-path spans removed before scoring. Reports
   * the kind and count only — never the matched text.
   */
  readonly redactions: readonly HelpRedaction[];
};

/** Raised when a caller pins a corpus digest that no longer matches. */
export class HelpCorpusDigestMismatchError extends Error {
  readonly expected: string;
  readonly actual: string;
  constructor(expected: string, actual: string) {
    super(`help retrieval: expected corpus ${expected} but this build ships ${actual}`);
    this.name = "HelpCorpusDigestMismatchError";
    this.expected = expected;
    this.actual = actual;
  }
}

/**
 * Article metadata vectors.
 *
 * Aliases and keywords are where paraphrase signal lives, so they get their
 * own point in the embedding space alongside the chunk vectors. Built once at
 * module load with the same fold-in the builder used.
 */
const ARTICLE_METADATA_VECTORS = new Map<string, Float64Array>();
for (const article of HELP_CORPUS.articles) {
  const tokens = tokenize(
    [
      article.title,
      ...article.aliases,
      ...article.keywords,
      ...article.localizations.flatMap((localization) => [localization.title, ...localization.keywords]),
    ].join(" \n "),
    4096,
  );
  const vector = embedHelpTokens(tokens);
  if (vector) ARTICLE_METADATA_VECTORS.set(article.id, vector);
}

function emptyOutcome(
  query: string,
  mode: HelpRetrievalMode,
  reason: HelpAbstentionReason,
  queryTruncated: boolean,
  confidence = 0,
  queryFamiliarity = 0,
  corrections: readonly { readonly from: string; readonly to: string }[] = [],
  redactions: readonly HelpRedaction[] = [],
): HelpRetrievalOutcome {
  return Object.freeze({
    schema: HELP_RETRIEVAL_SCHEMA,
    corpusDigest: HELP_CORPUS_DIGEST,
    corpusContentVersion: HELP_CORPUS.contentVersion,
    modelId: HELP_MODEL_ID,
    mode,
    query,
    results: Object.freeze([]),
    abstained: true,
    abstentionReason: reason,
    confidence,
    margin: 0,
    queryTruncated,
    queryFamiliarity,
    corrections: Object.freeze(corrections),
    redactions: Object.freeze(redactions),
  });
}

function citationsFor(articleId: string, chunkId: string): HelpCitation[] {
  const chunk = getHelpChunk(chunkId);
  const article = getHelpArticle(articleId);
  if (!chunk || !article) return [];
  const anchors = new Map<string, HelpSourceAnchor>(
    article.sources.map((source) => [source.id, source]),
  );
  return chunk.sourceIds
    .map((sourceId) => anchors.get(sourceId))
    .filter((anchor): anchor is HelpSourceAnchor => anchor !== undefined)
    .map((anchor) =>
      Object.freeze({
        sourceId: anchor.id,
        path: anchor.path,
        heading: anchor.heading,
        articleId,
        chunkId,
      }),
    );
}

function describe(components: HelpScoreComponents, matchedTerms: readonly string[]): string {
  const parts: string[] = [];
  if (components.exactPhrase > 0) parts.push("contains the exact query phrase");
  if (components.lexicalNormalized > 0) {
    parts.push(
      matchedTerms.length > 0
        ? `keyword match on ${matchedTerms.slice(0, 6).join(", ")} ` +
            `(BM25 ${components.lexicalBm25.toFixed(2)}, ` +
            `${Math.round(components.coordination * 100)}% of query terms)`
        : `keyword score ${components.lexicalBm25.toFixed(2)}`,
    );
  }
  if (components.semanticCosine > 0) {
    const discounted = components.semanticEffective < components.semanticCosine - 0.005;
    parts.push(
      discounted
        ? `semantic similarity ${components.semanticCosine.toFixed(2)} discounted to ` +
            `${components.semanticEffective.toFixed(2)} for unfamiliar query terms`
        : `semantic similarity ${components.semanticCosine.toFixed(2)}`,
    );
  }
  if (parts.length === 0) return "no contributing signal";
  return `${parts.join("; ")} — fused ${components.fused.toFixed(3)}`;
}

function isAuthorizedArticle(articleId: string, authorizedCapabilities: readonly string[] | undefined): boolean {
  const article = getHelpArticle(articleId);
  if (!article) return false;
  if (article.access === "public") return true;
  if (!authorizedCapabilities || authorizedCapabilities.length === 0) return false;
  const capabilities = new Set(authorizedCapabilities);
  return (
    article.capabilityIds.length > 0 &&
    article.capabilityIds.every((capability) => capabilities.has(capability))
  );
}

type Candidate = {
  articleId: string;
  bestChunkId: string | null;
  bestChunkFused: number;
  lexicalBm25: number;
  lexicalNormalized: number;
  exactPhrase: number;
  semanticCosine: number;
  fused: number;
  matchedTerms: Set<string>;
  locale: string;
  /** Locales that produced a lexical match, so citations stay in-language. */
  lexicalLocales: Set<string>;
};

/**
 * Search the canonical Help corpus.
 *
 * Deterministic: given the same corpus, model, query, and options, the result
 * order is identical on every run and every engine. Ties are broken by chunk
 * id, which is a total order, so no two results can swap between runs.
 */
export function searchHelpCorpus(
  rawQuery: string,
  options: HelpRetrievalOptions = {},
): HelpRetrievalOutcome {
  const mode = options.mode ?? "hybrid";
  // Redact first: a pasted credential must never be tokenized, scored,
  // echoed in an excerpt, or forwarded by the answer contract — and removing
  // it also stops a high-entropy string from consuming the query budget.
  const redaction = redactHelpText(typeof rawQuery === "string" ? rawQuery : "");
  const bounded = boundQuery(redaction.text);
  const queryTruncated = redaction.text.trim().length > bounded.length;

  if (options.expectCorpusDigest && options.expectCorpusDigest !== HELP_CORPUS_DIGEST) {
    throw new HelpCorpusDigestMismatchError(options.expectCorpusDigest, HELP_CORPUS_DIGEST);
  }
  if (options.signal?.aborted) return emptyOutcome(bounded, mode, "cancelled", queryTruncated, 0, 0, [], redaction.redactions);
  if (bounded.length === 0) return emptyOutcome(bounded, mode, "empty-query", queryTruncated, 0, 0, [], redaction.redactions);

  const limit = Math.max(1, Math.min(options.limit ?? HELP_RETRIEVAL_DEFAULT_LIMIT, HELP_RETRIEVAL_MAX_LIMIT));
  const queryTokens = tokenize(bounded, HELP_QUERY_MAX_TERMS);
  // Map tokens onto the vocabulary first, correcting likely misspellings, so
  // BM25, coordination, and the embedding fold-in all see real terms. A token
  // the corpus has no word for stays unresolved and counts against coverage.
  const resolvedTerms = resolveHelpQueryTerms(queryTokens);
  const effectiveTokens = resolvedTerms
    .map((entry) => entry.term)
    .filter((term): term is string => term !== null);
  const corrections = resolvedTerms
    .filter((entry) => entry.corrected)
    .map((entry) => Object.freeze({ from: entry.original, to: entry.term as string }));
  const distinctQueryTerms = new Set(resolvedTerms.map((entry) => entry.term ?? entry.original)).size;
  const normalizedQuery = normalizeText(bounded).replace(/\s+/g, " ").trim();
  if (queryTokens.length === 0) return emptyOutcome(bounded, mode, "no-match", queryTruncated, 0, 0, [], redaction.redactions);

  const useLexical = mode !== "semantic";
  const useSemantic = mode !== "lexical";
  // How much of this query the model has any basis to judge. Scales the
  // semantic component so a question about something the corpus never covers
  // cannot borrow confidence from vectors assembled out of unknown words.
  const queryFamiliarity = helpQueryFamiliarity(resolvedTerms);

  const candidates = new Map<string, Candidate>();
  const ensure = (articleId: string, locale: string): Candidate => {
    let candidate = candidates.get(articleId);
    if (!candidate) {
      candidate = {
        articleId,
        bestChunkId: null,
        bestChunkFused: -1,
        lexicalBm25: 0,
        lexicalNormalized: 0,
        exactPhrase: 0,
        semanticCosine: 0,
        fused: 0,
        matchedTerms: new Set<string>(),
        locale,
        lexicalLocales: new Set<string>(),
      };
      candidates.set(articleId, candidate);
    }
    return candidate;
  };

  // ---- lexical ------------------------------------------------------------
  if (useLexical) {
    if (options.signal?.aborted) return emptyOutcome(bounded, mode, "cancelled", queryTruncated, 0, queryFamiliarity, corrections, redaction.redactions);
    for (const hit of scoreLexical(effectiveTokens, normalizedQuery)) {
      const document: LexicalDocument = hit.document;
      const candidate = ensure(document.articleId, document.locale);
      const normalized = hit.bm25 / (hit.bm25 + BM25_SATURATION);
      if (normalized > candidate.lexicalNormalized) {
        candidate.lexicalNormalized = normalized;
        candidate.lexicalBm25 = hit.bm25;
      }
      if (hit.exactPhrase > candidate.exactPhrase) candidate.exactPhrase = hit.exactPhrase;
      for (const term of hit.matchedTerms) candidate.matchedTerms.add(term);
      if (document.locale !== "mul") candidate.lexicalLocales.add(document.locale);
      if (document.chunkId) {
        // Chunk-level evidence decides which chunk gets cited.
        const chunkScore = normalized + hit.exactPhrase;
        if (
          chunkScore > candidate.bestChunkFused ||
          (chunkScore === candidate.bestChunkFused &&
            candidate.bestChunkId !== null &&
            document.chunkId < candidate.bestChunkId)
        ) {
          candidate.bestChunkFused = chunkScore;
          candidate.bestChunkId = document.chunkId;
          candidate.locale = document.locale;
        }
      }
    }
  }

  // ---- semantic -----------------------------------------------------------
  if (useSemantic) {
    if (options.signal?.aborted) return emptyOutcome(bounded, mode, "cancelled", queryTruncated, 0, queryFamiliarity, corrections, redaction.redactions);
    const queryVector = embedHelpTokens(effectiveTokens);
    if (queryVector) {
      for (const chunk of HELP_CORPUS.chunks) {
        const chunkVector = helpChunkVector(chunk.id);
        if (!chunkVector) continue;
        const cosine = cosineSimilarity(queryVector, chunkVector);
        if (cosine <= 0) continue;
        const candidate = ensure(chunk.articleId, chunk.locale);
        if (cosine > candidate.semanticCosine) candidate.semanticCosine = cosine;
        // Only cite a chunk in a language the query actually reached: English
        // by default, or a locale that produced a lexical match. Otherwise a
        // purely semantic English query can end up citing a translated chunk.
        const citable = chunk.locale === "en" || candidate.lexicalLocales.has(chunk.locale);
        if (citable && candidate.bestChunkId === null && cosine > candidate.bestChunkFused) {
          candidate.bestChunkFused = cosine;
          candidate.bestChunkId = chunk.id;
          candidate.locale = chunk.locale;
        }
      }
      for (const [articleId, metadataVector] of ARTICLE_METADATA_VECTORS) {
        const cosine = cosineSimilarity(queryVector, metadataVector);
        if (cosine <= 0) continue;
        const candidate = ensure(articleId, "mul");
        if (cosine > candidate.semanticCosine) candidate.semanticCosine = cosine;
      }
    }
  }

  if (options.signal?.aborted) return emptyOutcome(bounded, mode, "cancelled", queryTruncated, 0, queryFamiliarity, corrections, redaction.redactions);
  if (candidates.size === 0) return emptyOutcome(bounded, mode, "no-match", queryTruncated, 0, queryFamiliarity, corrections, redaction.redactions);

  // ---- fuse, filter, rank -------------------------------------------------
  const weights = HELP_FUSION_WEIGHTS;
  // With a restricted mode the unused weights are redistributed so the fused
  // score stays on the same 0..1 scale and the threshold keeps its meaning.
  const activeWeight =
    (useLexical ? weights.lexical + weights.exactPhrase : 0) + (useSemantic ? weights.semantic : 0);

  const allowedAccess = options.access ? new Set(options.access) : null;
  const scored: Candidate[] = [];
  for (const candidate of candidates.values()) {
    const article = getHelpArticle(candidate.articleId);
    if (!article) continue;
    if (options.topic && options.topic !== "all" && article.topic !== options.topic) continue;
    if (options.audience && !article.audience.includes(options.audience)) continue;
    if (allowedAccess && !allowedAccess.has(article.access)) continue;
    if (!isAuthorizedArticle(article.id, options.authorizedCapabilities)) continue;
    // Coordination: how much of the query this article actually accounts for.
    // Without it a single rare term carries an otherwise unrelated query —
    // "convert 40 celsius to fahrenheit" matched an article containing
    // "converts" and scored high enough to answer.
    const coordination =
      distinctQueryTerms === 0 ? 0 : Math.min(1, candidate.matchedTerms.size / distinctQueryTerms);
    const dampedCoordination = Math.pow(coordination, HELP_COORDINATION_EXPONENT);
    const semanticEffective = candidate.semanticCosine * queryFamiliarity;
    const raw =
      (useLexical
        ? weights.lexical * candidate.lexicalNormalized * dampedCoordination +
          weights.exactPhrase * candidate.exactPhrase
        : 0) + (useSemantic ? weights.semantic * semanticEffective : 0);
    candidate.fused = activeWeight > 0 ? raw / activeWeight : 0;
    if (candidate.fused <= 0) continue;
    if (!candidate.bestChunkId) {
      // Cite the summary when only article metadata matched.
      candidate.bestChunkId = `${candidate.articleId}#en.summary.0`;
    }
    scored.push(candidate);
  }

  if (scored.length === 0) return emptyOutcome(bounded, mode, "no-match", queryTruncated, 0, queryFamiliarity, corrections, redaction.redactions);

  scored.sort(
    (left, right) =>
      right.fused - left.fused ||
      // Total, stable tie-break: no two candidates share an article id.
      (left.articleId < right.articleId ? -1 : left.articleId > right.articleId ? 1 : 0),
  );

  const confidence = scored[0]!.fused;
  const margin = scored.length > 1 ? confidence - scored[1]!.fused : confidence;
  const threshold = options.minConfidence ?? HELP_ABSTENTION_THRESHOLD;
  if (confidence < threshold) {
    return emptyOutcome(bounded, mode, "below-confidence", queryTruncated, confidence, queryFamiliarity, corrections, redaction.redactions);
  }

  const results: HelpRetrievalResult[] = [];
  for (const candidate of scored.slice(0, limit)) {
    const article = getHelpArticle(candidate.articleId)!;
    const chunk = getHelpChunk(candidate.bestChunkId!);
    const matchedTerms = [...candidate.matchedTerms].sort();
    const components: HelpScoreComponents = Object.freeze({
      lexicalBm25: candidate.lexicalBm25,
      lexicalNormalized: candidate.lexicalNormalized,
      coordination:
        distinctQueryTerms === 0 ? 0 : Math.min(1, candidate.matchedTerms.size / distinctQueryTerms),
      exactPhrase: candidate.exactPhrase,
      semanticCosine: candidate.semanticCosine,
      semanticEffective: candidate.semanticCosine * queryFamiliarity,
      fused: candidate.fused,
    });
    results.push(
      Object.freeze({
        rank: results.length + 1,
        articleId: article.id,
        chunkId: candidate.bestChunkId!,
        title: sanitizeHelpText(article.title, 256),
        topic: article.topic,
        summary: sanitizeHelpText(article.summary, 512),
        locale: chunk?.locale ?? "en",
        access: article.access,
        score: candidate.fused,
        components,
        matchedTerms: Object.freeze(matchedTerms),
        excerpt: buildHelpExcerpt(chunk?.text ?? article.summary, matchedTerms),
        citations: Object.freeze(citationsFor(article.id, candidate.bestChunkId!)),
        explanation: describe(components, matchedTerms),
      }),
    );
  }

  return Object.freeze({
    schema: HELP_RETRIEVAL_SCHEMA,
    corpusDigest: HELP_CORPUS_DIGEST,
    corpusContentVersion: HELP_CORPUS.contentVersion,
    modelId: HELP_MODEL_ID,
    mode,
    query: bounded,
    results: Object.freeze(results),
    abstained: false,
    abstentionReason: "none",
    confidence,
    margin,
    queryTruncated,
    queryFamiliarity,
    corrections: Object.freeze(corrections),
    redactions: Object.freeze(redaction.redactions),
  });
}
