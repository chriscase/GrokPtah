/**
 * Lexical retrieval: BM25 over canonical chunks plus exact-phrase matching.
 *
 * BM25 IDF comes from the model artifact, where it was computed at build time.
 * The runtime therefore never calls Math.log, and lexical scores are bit-stable
 * across engines.
 */
import { HELP_CORPUS } from "../canonical/corpus";
import type { HelpChunk } from "../canonical/types";
import { helpTermIdf } from "../model/artifact";
import { normalizeText, tokenize } from "./text";

/** Standard BM25 parameters; `b` is mild because chunks are already bounded. */
const BM25_K1 = 1.2;
const BM25_B = 0.6;

/** Title and summary text is a stronger relevance signal than body prose. */
const FIELD_WEIGHTS: Readonly<Record<HelpChunk["kind"], number>> = Object.freeze({
  title: 1.6,
  summary: 1.25,
  body: 1,
});

export type LexicalDocument = {
  readonly key: string;
  readonly articleId: string;
  /** Null for the per-article metadata document, which is not citable. */
  readonly chunkId: string | null;
  readonly kind: HelpChunk["kind"] | "metadata";
  readonly locale: string;
  readonly weight: number;
  readonly frequencies: ReadonlyMap<string, number>;
  readonly length: number;
  readonly normalizedText: string;
  readonly tokens: readonly string[];
};

function countTokens(tokens: readonly string[]): Map<string, number> {
  const frequencies = new Map<string, number>();
  for (const token of tokens) frequencies.set(token, (frequencies.get(token) ?? 0) + 1);
  return frequencies;
}

function buildDocuments(): LexicalDocument[] {
  const documents: LexicalDocument[] = [];
  for (const chunk of HELP_CORPUS.chunks) {
    const tokens = tokenize(chunk.text, 4096);
    documents.push({
      key: chunk.id,
      articleId: chunk.articleId,
      chunkId: chunk.id,
      kind: chunk.kind,
      locale: chunk.locale,
      weight: FIELD_WEIGHTS[chunk.kind],
      frequencies: countTokens(tokens),
      length: tokens.length,
      normalizedText: normalizeText(chunk.text),
      tokens,
    });
  }
  // Aliases and keywords carry the paraphrase and expert-terminology signal.
  // They are indexed for ranking but never cited, because they are lookup
  // metadata rather than source-backed prose.
  for (const article of HELP_CORPUS.articles) {
    const phrases = [
      ...article.aliases,
      ...article.keywords,
      ...article.localizations.flatMap((localization) => localization.keywords),
    ];
    const tokens = tokenize(phrases.join(" \n "), 4096);
    documents.push({
      key: `${article.id}#metadata`,
      articleId: article.id,
      chunkId: null,
      kind: "metadata",
      locale: "mul",
      weight: 1.35,
      frequencies: countTokens(tokens),
      length: tokens.length,
      normalizedText: normalizeText(phrases.join(" ")),
      tokens,
    });
  }
  return documents;
}

export const HELP_LEXICAL_DOCUMENTS: readonly LexicalDocument[] = Object.freeze(buildDocuments());

const AVERAGE_LENGTH =
  HELP_LEXICAL_DOCUMENTS.reduce((total, document) => total + document.length, 0) /
  Math.max(1, HELP_LEXICAL_DOCUMENTS.length);

/** Inverted index so scoring touches only documents that can actually match. */
const POSTINGS = new Map<string, LexicalDocument[]>();
for (const document of HELP_LEXICAL_DOCUMENTS) {
  for (const term of document.frequencies.keys()) {
    const bucket = POSTINGS.get(term);
    if (bucket) bucket.push(document);
    else POSTINGS.set(term, [document]);
  }
}

export type LexicalHit = {
  readonly document: LexicalDocument;
  readonly bm25: number;
  readonly exactPhrase: number;
  readonly matchedTerms: readonly string[];
};

/** BM25 contribution of one term in one document. */
function bm25Term(term: string, document: LexicalDocument): number {
  const frequency = document.frequencies.get(term);
  if (!frequency) return 0;
  const denominator =
    frequency + BM25_K1 * (1 - BM25_B + (BM25_B * document.length) / AVERAGE_LENGTH);
  return (helpTermIdf(term) * (frequency * (BM25_K1 + 1))) / denominator;
}

/**
 * Score every document that shares at least one term with the query.
 *
 * `exactPhrase` is a separate, reportable component: a verbatim occurrence of
 * the whole normalized query is strong evidence that keyword mode should win,
 * which is what keeps identifier lookups authoritative.
 */
export function scoreLexical(queryTokens: readonly string[], normalizedQuery: string): LexicalHit[] {
  if (queryTokens.length === 0) return [];
  const candidates = new Set<LexicalDocument>();
  for (const term of new Set(queryTokens)) {
    for (const document of POSTINGS.get(term) ?? []) candidates.add(document);
  }
  const hits: LexicalHit[] = [];
  for (const document of candidates) {
    let bm25 = 0;
    const matchedTerms: string[] = [];
    for (const term of new Set(queryTokens)) {
      const contribution = bm25Term(term, document);
      if (contribution > 0) {
        bm25 += contribution;
        matchedTerms.push(term);
      }
    }
    if (bm25 <= 0) continue;
    const exactPhrase =
      normalizedQuery.length >= 3 && document.normalizedText.includes(normalizedQuery) ? 1 : 0;
    hits.push({
      document,
      bm25: bm25 * document.weight,
      exactPhrase,
      matchedTerms: matchedTerms.sort(),
    });
  }
  return hits;
}

export const HELP_LEXICAL_STATS = Object.freeze({
  documentCount: HELP_LEXICAL_DOCUMENTS.length,
  averageLength: AVERAGE_LENGTH,
  vocabularySize: POSTINGS.size,
  k1: BM25_K1,
  b: BM25_B,
});
