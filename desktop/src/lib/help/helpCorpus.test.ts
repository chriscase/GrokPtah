/**
 * The corpus gates.
 *
 * The property that matters most here is that there is exactly *one*
 * hand-maintained corpus and both languages read the same bytes of it. The
 * rest is fail-closed behaviour: a corpus that does not match its digests must
 * not be served in a degraded form.
 */

import { describe, expect, it } from "vitest";

import fullCorpusJson from "./canonical/help-corpus.v1.json";
import publicCorpusJson from "./canonical/help-corpus-public.v1.json";
import {
  HELP_CORPUS,
  HELP_CORPUS_DIGEST,
  HelpCorpusDigestMismatchError,
  chunksForArticle,
  getHelpArticle,
  getHelpChunk,
  getHelpSource,
  isPublicOnly,
  verifyHelpCorpus,
} from "./canonical/corpus";
import type { HelpCorpus } from "./generated/contract";

function clone(corpus: HelpCorpus): HelpCorpus {
  return JSON.parse(JSON.stringify(corpus)) as HelpCorpus;
}

describe("the canonical corpus", () => {
  it("is the file Rust embeds, not a second copy", () => {
    // `HELP_CORPUS` is this exact JSON module, and the Rust host embeds the
    // same path with include_str!. If someone adds a second corpus file, this
    // identity check is the first thing that breaks.
    expect(HELP_CORPUS).toBe(fullCorpusJson as unknown as HelpCorpus);
  });

  it("verifies every digest at load", () => {
    expect(() => verifyHelpCorpus(HELP_CORPUS)).not.toThrow();
    expect(HELP_CORPUS_DIGEST).toMatch(/^sha256:[0-9a-f]{64}$/);
  });

  it("has articles, chunks, and sources that resolve to each other", () => {
    expect(HELP_CORPUS.articles.length).toBeGreaterThan(0);
    for (const article of HELP_CORPUS.articles) {
      expect(article.source_ids.length).toBeGreaterThan(0);
      for (const sourceId of article.source_ids) {
        expect(getHelpSource(sourceId)).toBeDefined();
      }
      expect(chunksForArticle(article.id).length).toBeGreaterThan(0);
    }
    for (const chunk of HELP_CORPUS.chunks) {
      expect(getHelpArticle(chunk.article_id)).toBeDefined();
      expect(getHelpChunk(chunk.id)).toBe(chunk);
    }
  });

  it("never lets an article be less restricted than a source it cites", () => {
    const rank = { public: 0, gated: 1, operator: 2 } as const;
    for (const article of HELP_CORPUS.articles) {
      for (const sourceId of article.source_ids) {
        const source = getHelpSource(sourceId);
        expect(source).toBeDefined();
        expect(rank[source!.visibility]).toBeLessThanOrEqual(rank[article.visibility]);
      }
    }
  });

  it("rejects a corpus whose chunk text was edited", () => {
    const tampered = clone(HELP_CORPUS);
    tampered.chunks = tampered.chunks.map((chunk, index) =>
      index === 0 ? { ...chunk, text: `${chunk.text} and one more thing` } : chunk,
    );
    expect(() => verifyHelpCorpus(tampered)).toThrow(HelpCorpusDigestMismatchError);
  });

  it("rejects a corpus whose source heading was edited", () => {
    const tampered = clone(HELP_CORPUS);
    tampered.sources = tampered.sources.map((source, index) =>
      index === 0 ? { ...source, heading: "Somewhere else" } : source,
    );
    expect(() => verifyHelpCorpus(tampered)).toThrow(HelpCorpusDigestMismatchError);
  });

  it("rejects a corpus whose visibility was widened without re-digesting", () => {
    // The attack this stops: relabel a gated source as public to get it into a
    // published bundle. Visibility is inside the digest, so it cannot be
    // changed quietly.
    const tampered = clone(HELP_CORPUS);
    const gated = tampered.sources.findIndex((source) => source.visibility !== "public");
    expect(gated).toBeGreaterThanOrEqual(0);
    tampered.sources = tampered.sources.map((source, index) =>
      index === gated ? { ...source, visibility: "public" } : source,
    );
    expect(() => verifyHelpCorpus(tampered)).toThrow(HelpCorpusDigestMismatchError);
  });

  it("rejects a corpus with an article silently removed", () => {
    const tampered = clone(HELP_CORPUS);
    tampered.articles = tampered.articles.slice(1);
    expect(() => verifyHelpCorpus(tampered)).toThrow(HelpCorpusDigestMismatchError);
  });
});

describe("the public bundle", () => {
  const publicCorpus = publicCorpusJson as unknown as HelpCorpus;

  it("is self-consistent on its own terms", () => {
    expect(() => verifyHelpCorpus(publicCorpus)).not.toThrow();
  });

  it("contains public records only", () => {
    expect(isPublicOnly(publicCorpus)).toBe(true);
    expect(publicCorpus.sources.length).toBeGreaterThan(0);
  });

  it("is smaller than the full corpus, and honest about being different", () => {
    expect(publicCorpus.articles.length).toBeLessThan(HELP_CORPUS.articles.length);
    expect(publicCorpus.digest).not.toBe(HELP_CORPUS.digest);
  });

  it("preserves record digests so a citation still verifies against the full corpus", () => {
    for (const source of publicCorpus.sources) {
      expect(source.digest).toBe(getHelpSource(source.id)?.digest);
    }
    for (const chunk of publicCorpus.chunks) {
      expect(chunk.digest).toBe(getHelpChunk(chunk.id)?.digest);
    }
  });

  it("does not carry the full corpus behind a filtered index", () => {
    // The whole point of filtering server-side: the restricted text must not
    // be present at all, not merely hidden from a list.
    const serialized = JSON.stringify(publicCorpus);
    const restricted = HELP_CORPUS.chunks.filter((chunk) => chunk.visibility !== "public");
    expect(restricted.length).toBeGreaterThan(0);
    for (const chunk of restricted) {
      expect(serialized).not.toContain(chunk.text);
    }
  });
});
