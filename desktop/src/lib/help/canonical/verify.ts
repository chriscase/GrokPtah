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

/**
 * Labels opening each variable-length region of a digest.
 *
 * Mirrors `region` in `crates/common/grokptah-help-contract/src/corpus.rs`.
 * Length prefixing makes a flat field list injective but does not record where
 * one sub-list ends and the next begins, so concatenating `aliases`,
 * `keywords` and `capability_ids` hashed the same bytes whichever list each
 * item came from — a capability could be moved into `aliases` and the article
 * and corpus digests stayed identical. Each region now carries its own label
 * and element count, so repartition, reorder, omission and duplication all
 * change the digest.
 */
const REGION = Object.freeze({
  aliases: "aliases",
  keywords: "keywords",
  capabilities: "capabilities",
  sources: "sources",
  articles: "articles",
  chunks: "chunks",
});

/** One labelled, counted region of a field list. */
function regionFields(label: string, items: readonly string[]): string[] {
  return [label, String(items.length), ...items];
}

function verifyArticle(
  article: HelpArticle,
  sources: ReadonlyMap<string, HelpSourceAnchor>,
): void {
  if (article.source_ids.length === 0) {
    throw new HelpCorpusDigestMismatchError(`article:${article.id}`, "at least one source", "empty");
  }
  const cited = article.source_ids.map((id) => {
    const source = sources.get(id);
    if (!source) {
      throw new HelpCorpusDigestMismatchError(`article:${article.id}`, id, "unknown source");
    }
    const rank = { public: 0, gated: 1, operator: 2 } as const;
    if (rank[source.visibility] > rank[article.visibility]) {
      throw new HelpCorpusDigestMismatchError(
        `article:${article.id}`,
        `visibility at least ${source.visibility}`,
        article.visibility,
      );
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
    ...regionFields(REGION.aliases, article.aliases),
    ...regionFields(REGION.keywords, article.keywords),
    ...regionFields(REGION.capabilities, article.capability_ids),
    ...regionFields(
      REGION.sources,
      cited.map((source) => source.digest),
    ),
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
  // Refuse ambiguity before Map construction or first-match lookup. A Map
  // keeps the last duplicate while Rust's iterator lookup keeps the first;
  // accepting duplicates therefore made visibility depend on the reader.
  const seen = new Set<string>();
  for (const [kind, records] of [
    ["source", corpus.sources],
    ["article", corpus.articles],
    ["chunk", corpus.chunks],
  ] as const) {
    for (const record of records) {
      if (seen.has(record.id)) {
        throw new HelpCorpusDigestMismatchError(
          `duplicate-${kind}:${record.id}`,
          "globally unique record id",
          record.id,
        );
      }
      seen.add(record.id);
    }
  }

  const sources = new Map(corpus.sources.map((source) => [source.id, source]));
  const articles = new Map(corpus.articles.map((article) => [article.id, article]));
  for (const source of corpus.sources) verifySource(source);
  for (const article of corpus.articles) verifyArticle(article, sources);
  for (const chunk of corpus.chunks) {
    // A chunk is only reachable through its article, and a consumer that
    // filters by article trusts the chunk to carry its article's visibility.
    // A document where the two disagree is served differently depending on
    // which of the two a reader's filter looks at, so it is refused.
    const article = articles.get(chunk.article_id);
    if (!article) {
      throw new HelpCorpusDigestMismatchError(
        `chunk:${chunk.id}`,
        chunk.article_id,
        "unknown article",
      );
    }
    if (article.visibility !== chunk.visibility) {
      throw new HelpCorpusDigestMismatchError(
        `chunk:${chunk.id}`,
        `article ${article.id} is ${article.visibility}`,
        `chunk is ${chunk.visibility}`,
      );
    }
    verifyChunk(chunk);
  }

  const sourceDigest = domainDigest(
    HELP_DIGEST_DOMAINS.sourceSet,
    regionFields(
      REGION.sources,
      corpus.sources.map((source) => source.digest),
    ),
  );
  if (sourceDigest !== corpus.source_digest) {
    throw new HelpCorpusDigestMismatchError("source-set", corpus.source_digest, sourceDigest);
  }

  const digest = domainDigest(HELP_DIGEST_DOMAINS.corpus, [
    corpus.schema_version,
    corpus.content_version,
    ...regionFields(
      REGION.articles,
      corpus.articles.map((article) => article.digest),
    ),
    ...regionFields(
      REGION.chunks,
      corpus.chunks.map((chunk) => chunk.digest),
    ),
    REGION.sources,
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
