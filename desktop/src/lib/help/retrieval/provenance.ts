/**
 * Reproducible retrieval-index provenance.
 *
 * The index is not a file on disk: it is derived at load from the canonical
 * corpus, the pinned model, the tokenizer, and the scoring parameters. Any of
 * those changing silently would change results while every individual digest
 * still looked correct, so they are hashed together into one value that a
 * caller can pin, a receipt can carry, and a stale-index check can compare.
 */
import { HELP_CORPUS, HELP_CORPUS_DIGEST } from "../canonical/corpus";
import { HELP_DIGEST_DOMAINS, domainDigest } from "../canonical/digest";
import { HELP_MODEL_PROVENANCE, HELP_MODEL_STATS } from "../model/artifact";
import { HELP_TOKENIZER_VERSION } from "./text";
import { HELP_LEXICAL_STATS } from "./lexical";

export const HELP_INDEX_SCHEMA = "grokptah.help-index-provenance.v1" as const;

export type HelpIndexProvenance = {
  readonly schema: typeof HELP_INDEX_SCHEMA;
  readonly indexDigest: string;
  readonly corpusDigest: string;
  readonly corpusContentVersion: string;
  readonly sourceDigest: string;
  readonly modelId: string;
  readonly modelDigest: string;
  readonly tokenizerVersion: string;
  readonly documentCount: number;
  readonly chunkCount: number;
  readonly articleCount: number;
  /** Every scoring constant that can change a ranking. */
  readonly scoring: Readonly<Record<string, number>>;
};

/**
 * Build the provenance record.
 *
 * Takes the scoring constants as an argument rather than importing them so
 * this module does not depend on `hybrid.ts`, which depends on it.
 */
export function buildHelpIndexProvenance(
  scoring: Readonly<Record<string, number>>,
): HelpIndexProvenance {
  const scoringFields = Object.keys(scoring)
    .sort()
    .flatMap((key) => [key, String(scoring[key])]);
  return Object.freeze({
    schema: HELP_INDEX_SCHEMA,
    indexDigest: domainDigest(HELP_DIGEST_DOMAINS.index, [
      HELP_CORPUS_DIGEST,
      HELP_CORPUS.sourceDigest,
      HELP_MODEL_PROVENANCE.sha256,
      HELP_TOKENIZER_VERSION,
      String(HELP_LEXICAL_STATS.documentCount),
      String(HELP_LEXICAL_STATS.k1),
      String(HELP_LEXICAL_STATS.b),
      ...scoringFields,
    ]),
    corpusDigest: HELP_CORPUS_DIGEST,
    corpusContentVersion: HELP_CORPUS.contentVersion,
    sourceDigest: HELP_CORPUS.sourceDigest,
    modelId: HELP_MODEL_STATS.modelId,
    modelDigest: HELP_MODEL_PROVENANCE.sha256,
    tokenizerVersion: HELP_TOKENIZER_VERSION,
    documentCount: HELP_LEXICAL_STATS.documentCount,
    chunkCount: HELP_CORPUS.chunks.length,
    articleCount: HELP_CORPUS.articles.length,
    scoring: Object.freeze({ ...scoring }),
  });
}
