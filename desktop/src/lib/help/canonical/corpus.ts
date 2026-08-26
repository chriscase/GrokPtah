/**
 * The one canonical corpus, as TypeScript reads it.
 *
 * `help-corpus.v1.json` is not a copy. It is the same file the Rust host
 * embeds with `include_str!`, generated from the Rust seed data by
 * `help-codegen`. There is one corpus document in this repository and both
 * languages read those bytes, so a rebuild cannot leave the two describing
 * different content.
 *
 * Verification is synchronous at module load and *fails closed*: if a stored
 * digest disagrees with the bytes it names, importing this module throws.
 * Returning a degraded corpus instead would mean answering from content that
 * nobody reviewed, which is the failure the digests exist to prevent.
 */

import corpusJson from "./help-corpus.v1.json";
import type {
  HelpArticle,
  HelpChunk,
  HelpCorpus,
  HelpSourceAnchor,
} from "../generated/contract";
import { HELP_DIGEST_DOMAINS, domainDigest } from "./digest";

/** Raised when the shipped corpus does not match its own digests. */
export class HelpCorpusDigestMismatchError extends Error {
  constructor(
    readonly record: string,
    readonly expected: string,
    readonly actual: string,
  ) {
    super(
      `Help corpus record ${record} does not match its digest. ` +
        `The shipped corpus is not the document its digest names, so nothing is served from it.`,
    );
    this.name = "HelpCorpusDigestMismatchError";
  }
}

function verifySource(source: HelpSourceAnchor): void {
  const actual = domainDigest(HELP_DIGEST_DOMAINS.source, [
    source.id,
    source.path,
    source.heading,
    source.visibility,
  ]);
  if (actual !== source.digest) {
    throw new HelpCorpusDigestMismatchError(`source:${source.id}`, source.digest, actual);
  }
}

function verifyChunk(chunk: HelpChunk): void {
  const actual = domainDigest(HELP_DIGEST_DOMAINS.chunk, [
    chunk.id,
    chunk.article_id,
    chunk.kind,
    String(chunk.ordinal),
    chunk.locale,
    chunk.text,
    chunk.visibility,
    ...chunk.source_ids,
  ]);
  if (actual !== chunk.digest) {
    throw new HelpCorpusDigestMismatchError(`chunk:${chunk.id}`, chunk.digest, actual);
  }
}

function verifyArticle(article: HelpArticle, sources: ReadonlyMap<string, HelpSourceAnchor>): void {
  const cited = article.source_ids.map((id) => {
    const source = sources.get(id);
    if (!source) {
      throw new HelpCorpusDigestMismatchError(`article:${article.id}`, id, "unknown source");
    }
    return source;
  });
  const actual = domainDigest(HELP_DIGEST_DOMAINS.article, [
    article.id,
    article.title,
    article.topic,
    article.summary,
    article.body,
    article.visibility,
    ...article.aliases,
    ...article.keywords,
    ...article.capability_ids,
    ...cited.map((source) => source.digest),
  ]);
  if (actual !== article.digest) {
    throw new HelpCorpusDigestMismatchError(`article:${article.id}`, article.digest, actual);
  }
}

/**
 * Recompute every digest in `corpus` and throw on the first disagreement.
 *
 * Exported so a consumer of the published package can re-run the same check on
 * whatever corpus it was handed, rather than trusting that the bundle it
 * loaded is the one that was published.
 */
export function verifyHelpCorpus(corpus: HelpCorpus): void {
  const sources = new Map(corpus.sources.map((source) => [source.id, source]));
  for (const source of corpus.sources) verifySource(source);
  for (const article of corpus.articles) verifyArticle(article, sources);
  for (const chunk of corpus.chunks) verifyChunk(chunk);

  const sourceDigest = domainDigest(
    HELP_DIGEST_DOMAINS.sourceSet,
    corpus.sources.map((source) => `${source.path}#${source.heading}`),
  );
  if (sourceDigest !== corpus.source_digest) {
    throw new HelpCorpusDigestMismatchError("source-set", corpus.source_digest, sourceDigest);
  }

  const digest = domainDigest(HELP_DIGEST_DOMAINS.corpus, [
    corpus.schema_version,
    corpus.content_version,
    ...corpus.articles.map((article) => article.digest),
    ...corpus.chunks.map((chunk) => chunk.digest),
    corpus.source_digest,
  ]);
  if (digest !== corpus.digest) {
    throw new HelpCorpusDigestMismatchError("corpus", corpus.digest, digest);
  }
}

/** The shipped corpus. Frozen, verified, and the only one. */
export const HELP_CORPUS: HelpCorpus = corpusJson as HelpCorpus;

verifyHelpCorpus(HELP_CORPUS);

export const HELP_CORPUS_DIGEST = HELP_CORPUS.digest;
export const HELP_CORPUS_CONTENT_VERSION = HELP_CORPUS.content_version;

const ARTICLES_BY_ID = new Map(HELP_CORPUS.articles.map((article) => [article.id, article]));
const CHUNKS_BY_ID = new Map(HELP_CORPUS.chunks.map((chunk) => [chunk.id, chunk]));
const SOURCES_BY_ID = new Map(HELP_CORPUS.sources.map((source) => [source.id, source]));

export function getHelpArticle(id: string): HelpArticle | undefined {
  return ARTICLES_BY_ID.get(id);
}

export function getHelpChunk(id: string): HelpChunk | undefined {
  return CHUNKS_BY_ID.get(id);
}

export function getHelpSource(id: string): HelpSourceAnchor | undefined {
  return SOURCES_BY_ID.get(id);
}

/** Chunks belonging to one article, in corpus order. */
export function chunksForArticle(articleId: string): readonly HelpChunk[] {
  return HELP_CORPUS.chunks.filter((chunk) => chunk.article_id === articleId);
}

/**
 * Whether this bundle contains only public sources.
 *
 * The published package asserts this rather than assuming it; a bundle that
 * carries a gated source has leaked it to every consumer at once.
 */
export function isPublicOnly(corpus: HelpCorpus = HELP_CORPUS): boolean {
  return (
    corpus.sources.every((source) => source.visibility === "public") &&
    corpus.articles.every((article) => article.visibility === "public") &&
    corpus.chunks.every((chunk) => chunk.visibility === "public")
  );
}
