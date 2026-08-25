import { describe, expect, it } from "vitest";
import {
  HELP_CORPUS,
  HELP_CORPUS_DIGEST,
  HELP_CHUNK_MAX_CHARS,
  getHelpArticle,
  getHelpChunk,
  getHelpSource,
  serializeHelpCorpus,
} from "./canonical/corpus";
import { canonicalDigest, canonicalJson, sha256Hex } from "./canonical/digest";
import {
  HELP_LEGACY_ENTRY_TO_ARTICLE,
  PROJECTED_HELP_ARTICLES,
  PROJECTED_HELP_ENTRIES,
} from "./canonical/projections";
import { HELP_SOURCE_REGISTRY } from "./canonical/data";
import CORPUS_LOCK from "./canonical/corpus.lock.json";

describe("canonical Help corpus", () => {
  it("is frozen all the way down", () => {
    expect(Object.isFrozen(HELP_CORPUS)).toBe(true);
    expect(Object.isFrozen(HELP_CORPUS.articles)).toBe(true);
    expect(Object.isFrozen(HELP_CORPUS.articles[0])).toBe(true);
    expect(Object.isFrozen(HELP_CORPUS.articles[0]!.sources)).toBe(true);
    expect(Object.isFrozen(HELP_CORPUS.chunks[0])).toBe(true);
    expect(() => (HELP_CORPUS.articles as unknown as unknown[]).push({})).toThrow();
  });

  it("gives every article at least one anchor that resolves to a registered source", () => {
    for (const article of HELP_CORPUS.articles) {
      expect(article.sources.length, article.id).toBeGreaterThan(0);
      for (const source of article.sources) {
        expect(HELP_SOURCE_REGISTRY[source.id], `${article.id} -> ${source.id}`).toBeDefined();
        expect(getHelpSource(source.id)?.path).toBe(source.path);
        expect(source.path.length).toBeGreaterThan(0);
        expect(source.heading.length).toBeGreaterThan(0);
      }
    }
  });

  it("keeps every citation id unambiguous", () => {
    // The two former corpora reused `product.readme`, `provider.profiles`, and
    // `computer-use.threat-model` across different headings, so a citation
    // could not identify which section backed a claim.
    const byId = new Map<string, string>();
    for (const source of HELP_CORPUS.sources) {
      const target = `${source.path}#${source.heading}`;
      if (byId.has(source.id)) expect(byId.get(source.id)).toBe(target);
      byId.set(source.id, target);
    }
    expect(byId.size).toBe(HELP_CORPUS.sources.length);
  });

  it("gives chunks stable, unique, bounded, cited identities", () => {
    const ids = new Set<string>();
    for (const chunk of HELP_CORPUS.chunks) {
      expect(ids.has(chunk.id), chunk.id).toBe(false);
      ids.add(chunk.id);
      expect(chunk.id).toBe(`${chunk.articleId}#${chunk.locale}.${chunk.kind}.${chunk.ordinal}`);
      expect(chunk.text.length).toBeGreaterThan(0);
      expect(chunk.text.length).toBeLessThanOrEqual(HELP_CHUNK_MAX_CHARS);
      expect(chunk.sourceIds.length).toBeGreaterThan(0);
      expect(getHelpArticle(chunk.articleId)).toBeDefined();
      expect(getHelpChunk(chunk.id)).toBe(chunk);
    }
  });

  it("serializes canonically and reproduces its digest", () => {
    expect(HELP_CORPUS_DIGEST).toMatch(/^sha256:[0-9a-f]{64}$/);
    expect(
      canonicalDigest({
        schemaVersion: HELP_CORPUS.schemaVersion,
        contentVersion: HELP_CORPUS.contentVersion,
        articles: HELP_CORPUS.articles,
        chunks: HELP_CORPUS.chunks,
      }),
    ).toBe(HELP_CORPUS_DIGEST);
    expect(`sha256:${sha256Hex(serializeHelpCorpus())}`).toBe(
      `sha256:${CORPUS_LOCK.serializationSha256}`,
    );
  });

  it("matches the committed digest lock", () => {
    // Corpus drift has to be a reviewed change, not a silent one.
    expect(HELP_CORPUS.digest).toBe(CORPUS_LOCK.digest);
    expect(HELP_CORPUS.sourceDigest).toBe(CORPUS_LOCK.sourceDigest);
    expect(HELP_CORPUS.articles.length).toBe(CORPUS_LOCK.articleCount);
    expect(HELP_CORPUS.chunks.length).toBe(CORPUS_LOCK.chunkCount);
    expect(HELP_CORPUS.sources.length).toBe(CORPUS_LOCK.sourceCount);
  });

  it("serializes with sorted keys so equal content always digests equally", () => {
    expect(canonicalJson({ b: 1, a: { d: 2, c: 3 } })).toBe('{"a":{"c":3,"d":2},"b":1}');
    expect(canonicalDigest({ x: 1, y: 2 })).toBe(canonicalDigest({ y: 2, x: 1 }));
  });

  it("projects both legacy contracts without losing content", () => {
    expect(PROJECTED_HELP_ARTICLES.length).toBe(HELP_CORPUS.articles.length);
    for (const entry of PROJECTED_HELP_ENTRIES) {
      const articleId = HELP_LEGACY_ENTRY_TO_ARTICLE[entry.id];
      expect(articleId, entry.id).toBeDefined();
      const article = getHelpArticle(articleId!);
      expect(article?.body).toBe(entry.body);
      expect(article?.access).toBe(entry.access);
      expect([...entry.capabilityIds]).toEqual([...(article?.capabilityIds ?? [])]);
    }
  });

  it("keeps every capability covered by the entry projection", () => {
    const covered = new Set(PROJECTED_HELP_ENTRIES.flatMap((entry) => entry.capabilityIds));
    expect(covered).toEqual(
      new Set([
        "session.observe",
        "run.execute",
        "run.queue",
        "run.review",
        "run.promote",
        "agent.continuity",
        "agent.resume",
        "computer.observe",
        "computer.control",
      ]),
    );
  });

  it("ships no secret or private-path text", () => {
    const patterns = [
      /\bxai-[A-Za-z0-9]{8,}/i,
      /\bsk-[A-Za-z0-9]{16,}/,
      /Authorization:\s*Bearer\s+\S+/i,
      /-----BEGIN [A-Z ]*PRIVATE KEY-----/,
      /\/Users\/[A-Za-z0-9._-]+\//,
      /\/home\/[A-Za-z0-9._-]+\//,
      /\bGROKPTAH_HOME\b/,
    ];
    for (const article of HELP_CORPUS.articles) {
      const blob = [article.title, article.summary, article.body, ...article.aliases, ...article.keywords].join("\n");
      for (const pattern of patterns) {
        expect(pattern.test(blob), `${article.id} matched ${pattern}`).toBe(false);
      }
    }
  });

  it("orders articles by topic and keeps the introductory article first", () => {
    const topics = HELP_CORPUS.articles.map((article) => article.topic);
    const firstIndex = new Map<string, number>();
    topics.forEach((topic, index) => {
      if (!firstIndex.has(topic)) firstIndex.set(topic, index);
    });
    // Each topic occupies one contiguous run.
    for (const [topic, start] of firstIndex) {
      const count = topics.filter((candidate) => candidate === topic).length;
      expect(topics.slice(start, start + count).every((candidate) => candidate === topic)).toBe(true);
    }
    expect(HELP_CORPUS.articles[0]!.id).toBe("getting-started.sessions");
  });
});
