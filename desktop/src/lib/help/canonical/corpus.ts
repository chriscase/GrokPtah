/**
 * Builds the frozen, digest-bound canonical corpus from the authored seeds.
 *
 * Chunk IDs are derived from `articleId`, locale, kind, and ordinal, so they
 * are stable across rebuilds and can be cited verbatim by an answer contract.
 */
import { HELP_DIGEST_DOMAINS, canonicalDigest, canonicalJson, domainDigest } from "./digest";
import { HELP_ARTICLE_SEEDS, HELP_SOURCE_REGISTRY } from "./data";
import {
  HELP_CANONICAL_CONTENT_VERSION,
  HELP_CANONICAL_SCHEMA_VERSION,
  type HelpCanonicalArticle,
  type HelpCanonicalCorpus,
  type HelpChunk,
  type HelpSourceAnchor,
  type HelpSourceVisibility,
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

type UndigestedChunk = Omit<HelpChunk, "digest">;

function buildChunks(article: HelpCanonicalArticle): UndigestedChunk[] {
  const sourceIds = Object.freeze(article.sources.map((source) => source.id));
  const chunks: UndigestedChunk[] = [
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

/**
 * Resolve a citation id to an anchor carrying its own digest.
 *
 * Visibility defaults to `public` here only because every shipped source is
 * public documentation; `project` and `private` sources are default-deny at
 * the authority layer, which is where the decision belongs.
 */
function resolveSource(sourceId: string, articleId: string): HelpSourceAnchor {
  const seed = HELP_SOURCE_REGISTRY[sourceId];
  if (!seed) throw new Error(`help corpus: article ${articleId} cites unknown source ${sourceId}`);
  const visibility: HelpSourceVisibility = seed.visibility ?? "public";
  return Object.freeze({
    id: seed.id,
    path: seed.path,
    heading: seed.heading,
    visibility,
    digest: domainDigest(HELP_DIGEST_DOMAINS.source, [seed.id, seed.path, seed.heading, visibility]),
  });
}

function resolveArticle(seed: (typeof HELP_ARTICLE_SEEDS)[number]): HelpCanonicalArticle {
  if (seed.sourceIds.length === 0) {
    throw new Error(`help corpus: article ${seed.id} has no source anchor`);
  }
  if (seed.body.length > HELP_ARTICLE_MAX_BODY_CHARS) {
    throw new Error(`help corpus: article ${seed.id} body exceeds ${HELP_ARTICLE_MAX_BODY_CHARS} characters`);
  }
  const sources = seed.sourceIds.map((sourceId) => resolveSource(sourceId, seed.id));
  const { sourceIds: _unused, ...rest } = seed;
  return Object.freeze({
    ...rest,
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
    digest: domainDigest(HELP_DIGEST_DOMAINS.article, [
      seed.id,
      seed.title,
      seed.topic,
      seed.summary,
      seed.body,
      seed.access,
      ...seed.audience,
      ...seed.capabilityIds,
      ...seed.aliases,
      ...seed.keywords,
      ...sources.map((source) => source.digest),
    ]),
  }) as HelpCanonicalArticle;
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
        digest: domainDigest(HELP_DIGEST_DOMAINS.chunk, [
          chunk.id,
          chunk.articleId,
          chunk.kind,
          String(chunk.ordinal),
          chunk.locale,
          chunk.text,
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

  // Reuse the anchors the articles already resolved, so a source and its
  // citation can never disagree about visibility or digest.
  const byId = new Map<string, HelpSourceAnchor>();
  for (const article of articles) {
    for (const source of article.sources) byId.set(source.id, source);
  }
  const sources: HelpSourceAnchor[] = [...byId.keys()].sort(compare).map((id) => byId.get(id)!);

  // Content digest covers everything a consumer can observe; the source digest
  // covers only the cited anchors so anchor drift is separately detectable.
  const digest = canonicalDigest({
    schemaVersion: HELP_CANONICAL_SCHEMA_VERSION,
    contentVersion: HELP_CANONICAL_CONTENT_VERSION,
    articles,
    chunks,
  });
  // Over per-source digests. The previous form joined `id|path#heading`, which
  // collides whenever a separator character appears inside a field.
  const sourceDigest = domainDigest(
    HELP_DIGEST_DOMAINS.sourceSet,
    sources.map((source) => source.digest),
  );

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
  return canonicalJson({
    schemaVersion: HELP_CORPUS.schemaVersion,
    contentVersion: HELP_CORPUS.contentVersion,
    articles: HELP_CORPUS.articles,
    chunks: HELP_CORPUS.chunks,
  });
}
