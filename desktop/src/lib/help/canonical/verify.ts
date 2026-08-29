/**
 * Corpus verification, with no corpus attached.
 *
 * These functions take the corpus they check. That is not a style preference:
 * a module that *imports* a corpus in order to verify one drags those bytes
 * into every bundle that imports the verifier. That is exactly how the private
 * corpus first reached the published package — `publicSurface` imported
 * `verifyHelpCorpus` from a module that loaded the full corpus at the top
 * level, and all 27 restricted chunks were bundled even though none were
 * exported. Export lists do not describe what a bundler emits.
 *
 * So verification lives here, alone, and the corpora live in modules that only
 * the side entitled to them imports.
 */

import type { HelpArticle, HelpChunk, HelpCorpus, HelpSourceAnchor } from "../generated/contract";
import { HELP_DIGEST_DOMAINS, domainDigest } from "./digest";

/** Raised when a corpus does not match its own digests. */
export class HelpCorpusDigestMismatchError extends Error {
  constructor(
    readonly record: string,
    readonly expected: string,
    readonly actual: string,
  ) {
    super(
      `Help corpus record ${record} does not match its digest. ` +
        `The corpus is not the document its digest names, so nothing is served from it.`,
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

function verifyArticle(
  article: HelpArticle,
  sources: ReadonlyMap<string, HelpSourceAnchor>,
): void {
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
 * Exported so a consumer can re-run the check on whatever corpus it was
 * handed, rather than trusting that the bundle it loaded is the one that was
 * published, or that the corpus a server sent is the one that server claims.
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

/** Whether every record in `corpus` is public. */
export function isPublicOnly(corpus: HelpCorpus): boolean {
  return (
    corpus.sources.every((source) => source.visibility === "public") &&
    corpus.articles.every((article) => article.visibility === "public") &&
    corpus.chunks.every((chunk) => chunk.visibility === "public")
  );
}

/** Look up an article by id within a given corpus. */
export function findArticle(corpus: HelpCorpus, id: string): HelpArticle | undefined {
  return corpus.articles.find((article) => article.id === id);
}

/** Look up a chunk by id within a given corpus. */
export function findChunk(corpus: HelpCorpus, id: string): HelpChunk | undefined {
  return corpus.chunks.find((chunk) => chunk.id === id);
}

/** Look up a source anchor by id within a given corpus. */
export function findSource(corpus: HelpCorpus, id: string): HelpSourceAnchor | undefined {
  return corpus.sources.find((source) => source.id === id);
}

/** Chunks belonging to one article, in corpus order. */
export function chunksForArticle(corpus: HelpCorpus, articleId: string): readonly HelpChunk[] {
  return corpus.chunks.filter((chunk) => chunk.article_id === articleId);
}
