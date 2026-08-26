/**
 * Builds the frozen, digest-bound canonical corpus from the authored seeds.
 *
 * Chunk IDs are derived from `articleId`, locale, kind, and ordinal, so they
 * are stable across rebuilds and can be cited verbatim by an answer contract.
 */
import { HELP_DIGEST_DOMAINS, canonicalJson, domainDigest } from "./digest";
import { HELP_ARTICLE_SEEDS, HELP_SOURCE_REGISTRY } from "./data";
import {
  HELP_CANONICAL_CONTENT_VERSION,
  HELP_CANONICAL_SCHEMA_VERSION,
  type HelpCanonicalArticle,
  type HelpCanonicalCorpus,
  type HelpChunk,
  type HelpSourceAnchor,
  type HelpTopic,
} from "./types";

/** Retrieval and citation bounds. Enforced at build time, not just documented. */
export const HELP_CHUNK_MAX_CHARS = 512;
export const HELP_ARTICLE_MAX_BODY_CHARS = 4_096;
export const HELP_MAX_ARTICLES = 512;

/** Reading order for topics; also fixes the canonical article order. */
export const HELP_TOPIC_ORDER: readonly HelpTopic[] = Object.freeze([
  "getting-started",
  "providers",
  "computer-use",
  "operations",
]);

function compare(left: string, right: string): number {
  return left < right ? -1 : left > right ? 1 : 0;
}

/**
 * Split prose on sentence boundaries, then pack sentences into chunks that
 * stay under `HELP_CHUNK_MAX_CHARS`. A single over-long sentence is hard-split
 * so a chunk can never exceed the bound.
 */
function splitIntoChunkTexts(body: string): string[] {
  const sentences = body
    .split(/(?<=[.!?])\s+/u)
    .map((sentence) => sentence.trim())
    .filter((sentence) => sentence.length > 0);
  const chunks: string[] = [];
  let current = "";
  for (const sentence of sentences) {
    if (sentence.length > HELP_CHUNK_MAX_CHARS) {
      if (current) { chunks.push(current); current = ""; }
      for (let index = 0; index < sentence.length; index += HELP_CHUNK_MAX_CHARS) {
        chunks.push(sentence.slice(index, index + HELP_CHUNK_MAX_CHARS));
      }
      continue;
    }
    const candidate = current ? `${current} ${sentence}` : sentence;
    if (candidate.length > HELP_CHUNK_MAX_CHARS) {
      if (current) chunks.push(current);
      current = sentence;
    } else {
      current = candidate;
    }
  }
  if (current) chunks.push(current);
  return chunks;
}

function chunkId(articleId: string, locale: string, kind: HelpChunk["kind"], ordinal: number): string {
  return `${articleId}#${locale}.${kind}.${ordinal}`;
}

/** A chunk before its digest is computed; `buildCorpus` seals them. */
type HelpChunkSeed = Omit<HelpChunk, "digest">;

function buildChunks(article: HelpCanonicalArticle): HelpChunkSeed[] {
  const sourceIds = Object.freeze(article.sources.map((source) => source.id));
  const chunks: HelpChunkSeed[] = [
    {
      id: chunkId(article.id, "en", "title", 0),
      articleId: article.id,
      kind: "title",
      ordinal: 0,
      text: article.title,
      locale: "en",
      sourceIds,
    },
    {
      id: chunkId(article.id, "en", "summary", 0),
      articleId: article.id,
      kind: "summary",
      ordinal: 0,
      text: article.summary,
      locale: "en",
      sourceIds,
    },
  ];
  splitIntoChunkTexts(article.body).forEach((text, ordinal) => {
    chunks.push({
      id: chunkId(article.id, "en", "body", ordinal),
      articleId: article.id,
      kind: "body",
      ordinal,
      text,
      locale: "en",
      sourceIds,
    });
  });
  // Localized surface text is retrievable in-language and cites the same
  // sources; it is a translation of the article, not a separate claim.
  for (const localization of article.localizations) {
    chunks.push({
      id: chunkId(article.id, localization.locale, "title", 0),
      articleId: article.id,
      kind: "title",
      ordinal: 0,
      text: localization.title,
      locale: localization.locale,
      sourceIds,
    });
    chunks.push({
      id: chunkId(article.id, localization.locale, "summary", 0),
      articleId: article.id,
      kind: "summary",
      ordinal: 0,
      text: localization.summary,
      locale: localization.locale,
      sourceIds,
    });
  }
  return chunks;
}

function resolveArticle(seed: (typeof HELP_ARTICLE_SEEDS)[number]): HelpCanonicalArticle {
  if (seed.sourceIds.length === 0) {
    throw new Error(`help corpus: article ${seed.id} has no source anchor`);
  }
  if (seed.body.length > HELP_ARTICLE_MAX_BODY_CHARS) {
    throw new Error(`help corpus: article ${seed.id} body exceeds ${HELP_ARTICLE_MAX_BODY_CHARS} characters`);
  }
  const sources = seed.sourceIds.map((sourceId) => {
    const anchor = HELP_SOURCE_REGISTRY[sourceId];
    if (!anchor) throw new Error(`help corpus: article ${seed.id} cites unknown source ${sourceId}`);
    return Object.freeze({
      ...anchor,
      digest: domainDigest(HELP_DIGEST_DOMAINS.source, [anchor.id, anchor.path, anchor.heading]),
    });
  });
  const { sourceIds: _unused, ...rest } = seed;
  // Each article carries its own digest, so the corpus digest is a digest of
  // digests: a consumer that sees the corpus move can say which article moved.
  const digest = domainDigest(HELP_DIGEST_DOMAINS.article, [
    seed.id,
    seed.title,
    seed.topic,
    seed.summary,
    seed.body,
    seed.access,
    String(seed.aliases.length),
    ...seed.aliases,
    String(seed.keywords.length),
    ...seed.keywords,
    String(seed.audience.length),
    ...seed.audience,
    String(seed.capabilityIds.length),
    ...seed.capabilityIds,
    String(sources.length),
    ...sources.map((source) => source.digest),
    String(seed.localizations.length),
    ...seed.localizations.flatMap((localization) => [
      localization.locale,
      localization.title,
      localization.summary,
      String(localization.keywords.length),
      ...localization.keywords,
    ]),
  ]);
  return Object.freeze({
    ...rest,
    digest,
    aliases: Object.freeze([...seed.aliases]),
    keywords: Object.freeze([...seed.keywords]),
    audience: Object.freeze([...seed.audience]),
    capabilityIds: Object.freeze([...seed.capabilityIds]),
    localizations: Object.freeze(
      seed.localizations.map((localization) =>
        Object.freeze({ ...localization, keywords: Object.freeze([...localization.keywords]) }),
      ),
    ),
    sources: Object.freeze(sources),
  }) as HelpCanonicalArticle;
}

/** The exact bytes the corpus digest covers. */
function serializeCorpusRecord(
  articles: readonly HelpCanonicalArticle[],
  chunks: readonly HelpChunk[],
  sources: readonly HelpSourceAnchor[],
): string {
  return canonicalJson({
    schemaVersion: HELP_CANONICAL_SCHEMA_VERSION,
    contentVersion: HELP_CANONICAL_CONTENT_VERSION,
    articles,
    chunks,
    sources,
  });
}

function buildCorpus(): HelpCanonicalCorpus {
  if (HELP_ARTICLE_SEEDS.length > HELP_MAX_ARTICLES) {
    throw new Error(`help corpus: more than ${HELP_MAX_ARTICLES} articles`);
  }
  // Ordered by topic, then by the order the articles are authored in `data.ts`.
  // Deterministic — the seed file is fixed, and any reorder shows up as a lock
  // diff — while still reading in a sensible sequence. Sorting by id would put
  // computer-use first and, within getting-started, lead with accessibility
  // rather than the introductory article, purely because of the alphabet.
  const seedOrder = new Map(HELP_ARTICLE_SEEDS.map((seed, index) => [seed.id, index]));
  const articles = HELP_ARTICLE_SEEDS.map(resolveArticle)
    .slice()
    .sort(
      (left, right) =>
        HELP_TOPIC_ORDER.indexOf(left.topic) - HELP_TOPIC_ORDER.indexOf(right.topic) ||
        (seedOrder.get(left.id) ?? 0) - (seedOrder.get(right.id) ?? 0),
    );

  const ids = new Set<string>();
  const legacyIds = new Set<string>();
  for (const article of articles) {
    if (ids.has(article.id)) throw new Error(`help corpus: duplicate article id ${article.id}`);
    ids.add(article.id);
    if (article.legacyEntryId) {
      if (legacyIds.has(article.legacyEntryId)) {
        throw new Error(`help corpus: duplicate legacy entry id ${article.legacyEntryId}`);
      }
      legacyIds.add(article.legacyEntryId);
    }
  }

  const chunks = articles
    .flatMap(buildChunks)
    .slice()
    .sort((left, right) => compare(left.id, right.id))
    .map((chunk) =>
      Object.freeze({
        ...chunk,
        sourceIds: Object.freeze([...chunk.sourceIds]),
        // Bound to the bytes, not to the id. A citation span carries this so a
        // rebuilt corpus invalidates the span instead of silently re-pointing
        // it at different words.
        digest: domainDigest(HELP_DIGEST_DOMAINS.chunk, [
          chunk.id,
          chunk.articleId,
          chunk.kind,
          String(chunk.ordinal),
          chunk.locale,
          chunk.text,
          String(chunk.sourceIds.length),
          ...chunk.sourceIds,
        ]),
      }),
    );

  const chunkIds = new Set<string>();
  for (const chunk of chunks) {
    if (chunkIds.has(chunk.id)) throw new Error(`help corpus: duplicate chunk id ${chunk.id}`);
    if (chunk.text.length > HELP_CHUNK_MAX_CHARS) {
      throw new Error(`help corpus: chunk ${chunk.id} exceeds ${HELP_CHUNK_MAX_CHARS} characters`);
    }
    if (chunk.sourceIds.length === 0) throw new Error(`help corpus: chunk ${chunk.id} has no citation`);
    chunkIds.add(chunk.id);
  }

  const usedSourceIds = new Set(articles.flatMap((article) => article.sources.map((source) => source.id)));
  const sources: HelpSourceAnchor[] = [...usedSourceIds].sort(compare).map((sourceId) => {
    const seed = HELP_SOURCE_REGISTRY[sourceId]!;
    return Object.freeze({
      ...seed,
      digest: domainDigest(HELP_DIGEST_DOMAINS.source, [seed.id, seed.path, seed.heading]),
    });
  });

  // Content digest covers everything a consumer can observe; the source digest
  // covers only the cited anchors so anchor drift is separately detectable.
  //
  // Both are length-prefixed rather than separator-joined. The previous source
  // digest joined `${id}|${path}#${heading}`, which is not injective: a source
  // whose id contains `|` produces the same string as a different source whose
  // path begins with the rest of it, and the two hash identically.
  //
  // The corpus digest is taken over the canonical serialization rather than
  // over a field list, so `serializeHelpCorpus()` remains exactly the bytes it
  // covers: an external verifier hashes one string instead of re-implementing
  // the record rules. Those per-record digests are inside that serialization,
  // so a substitution anywhere moves both the record's digest and the corpus's.
  const digest = domainDigest(HELP_DIGEST_DOMAINS.corpus, [
    serializeCorpusRecord(articles, chunks, sources),
  ]);
  const sourceDigest = domainDigest(HELP_DIGEST_DOMAINS.sourceSet, [
    String(sources.length),
    ...sources.map((source) => source.digest),
  ]);

  return Object.freeze({
    schemaVersion: HELP_CANONICAL_SCHEMA_VERSION,
    contentVersion: HELP_CANONICAL_CONTENT_VERSION,
    articles: Object.freeze(articles),
    chunks: Object.freeze(chunks),
    sources: Object.freeze(sources),
    digest,
    sourceDigest,
  });
}

/** The frozen canonical corpus. Everything else is a projection of this. */
export const HELP_CORPUS: HelpCanonicalCorpus = buildCorpus();

/** Stable digest of the shipped corpus; retrieval and answers bind to it. */
export const HELP_CORPUS_DIGEST = HELP_CORPUS.digest;

const ARTICLES_BY_ID = new Map(HELP_CORPUS.articles.map((article) => [article.id, article]));
const CHUNKS_BY_ID = new Map(HELP_CORPUS.chunks.map((chunk) => [chunk.id, chunk]));
const SOURCES_BY_ID = new Map(HELP_CORPUS.sources.map((source) => [source.id, source]));

export function getHelpArticle(articleId: string): HelpCanonicalArticle | undefined {
  return ARTICLES_BY_ID.get(articleId);
}

export function getHelpChunk(chunkId: string): HelpChunk | undefined {
  return CHUNKS_BY_ID.get(chunkId);
}

export function getHelpSource(sourceId: string): HelpSourceAnchor | undefined {
  return SOURCES_BY_ID.get(sourceId);
}

/**
 * The exact bytes the digest is computed over. Exposed so an external verifier
 * can recompute the digest without re-implementing the serialization rules.
 */
export function serializeHelpCorpus(): string {
  return serializeCorpusRecord(HELP_CORPUS.articles, HELP_CORPUS.chunks, HELP_CORPUS.sources);
}

/** Recompute the corpus digest from the published serialization. */
export function recomputeHelpCorpusDigest(serialized: string): string {
  return domainDigest(HELP_DIGEST_DOMAINS.corpus, [serialized]);
}
