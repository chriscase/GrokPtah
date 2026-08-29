/**
 * Corpus gates.
 *
 * Two properties matter here. There is exactly one hand-maintained corpus, and
 * both languages read the same bytes of it — Rust embeds
 * `help-corpus.v1.json` with `include_str!` and this file reads that same
 * artifact. And the *bundle* ships only the public subset: TypeScript app code
 * never imports the full corpus, because importing it anywhere under
 * `src/lib` drags the restricted text into every bundle downstream.
 *
 * Test files are not bundled, so this file may read the private artifact
 * directly. `scripts/verify-public.mjs` asserts the emitted bundles do not.
 */

import { describe, expect, it } from "vitest";

// Read directly: `src/lib` must not import this, and a test is the one place
// that can hold both corpora at once to compare them.
import fullCorpusJson from "./canonical/help-corpus.v1.json";
import publicCorpusJson from "./canonical/help-corpus-public.v1.json";
import {
  HELP_CORPUS,
  HELP_PUBLIC_CORPUS,
  HELP_PUBLIC_CORPUS_DIGEST,
  chunksForPublicArticle,
  getHelpArticle,
  getHelpChunk,
  getHelpSource,
} from "./canonical/corpus";
import {
  HelpCorpusDigestMismatchError,
  findSource,
  isPublicOnly,
  verifyHelpCorpus,
} from "./canonical/verify";
import type { HelpCorpus } from "./generated/contract";

const FULL = fullCorpusJson as unknown as HelpCorpus;
const PUBLIC = publicCorpusJson as unknown as HelpCorpus;

function clone(corpus: HelpCorpus): HelpCorpus {
  return JSON.parse(JSON.stringify(corpus)) as HelpCorpus;
}

describe("the one canonical corpus", () => {
  it("verifies every digest, in the same way Rust does", () => {
    expect(() => verifyHelpCorpus(FULL)).not.toThrow();
    expect(FULL.digest).toMatch(/^sha256:[0-9a-f]{64}$/);
  });

  it("has articles, chunks, and sources that resolve to each other", () => {
    expect(FULL.articles.length).toBeGreaterThan(0);
    for (const article of FULL.articles) {
      expect(article.source_ids.length).toBeGreaterThan(0);
      for (const sourceId of article.source_ids) {
        expect(findSource(FULL, sourceId)).toBeDefined();
      }
      expect(FULL.chunks.some((chunk) => chunk.article_id === article.id)).toBe(true);
    }
    for (const chunk of FULL.chunks) {
      expect(FULL.articles.some((article) => article.id === chunk.article_id)).toBe(true);
    }
  });

  it("never lets an article be less restricted than a source it cites", () => {
    // Otherwise a public reader learns that a gated document exists from the
    // citation alone.
    const rank = { public: 0, gated: 1, operator: 2 } as const;
    for (const article of FULL.articles) {
      for (const sourceId of article.source_ids) {
        const source = findSource(FULL, sourceId);
        expect(source).toBeDefined();
        expect(rank[source!.visibility]).toBeLessThanOrEqual(rank[article.visibility]);
      }
    }
  });

  it("rejects a corpus whose chunk text was edited", () => {
    const tampered = clone(FULL);
    tampered.chunks = tampered.chunks.map((chunk, index) =>
      index === 0 ? { ...chunk, text: `${chunk.text} and one more thing` } : chunk,
    );
    expect(() => verifyHelpCorpus(tampered)).toThrow(HelpCorpusDigestMismatchError);
  });

  it("rejects a corpus whose source heading was edited", () => {
    const tampered = clone(FULL);
    tampered.sources = tampered.sources.map((source, index) =>
      index === 0 ? { ...source, heading: "Somewhere else" } : source,
    );
    expect(() => verifyHelpCorpus(tampered)).toThrow(HelpCorpusDigestMismatchError);
  });

  it("rejects a corpus whose visibility was widened without re-digesting", () => {
    // The attack this stops: relabel a gated source public to get it into a
    // published bundle. Visibility is inside the digest.
    const tampered = clone(FULL);
    const gated = tampered.sources.findIndex((source) => source.visibility !== "public");
    expect(gated).toBeGreaterThanOrEqual(0);
    tampered.sources = tampered.sources.map((source, index) =>
      index === gated ? { ...source, visibility: "public" } : source,
    );
    expect(() => verifyHelpCorpus(tampered)).toThrow(HelpCorpusDigestMismatchError);
  });

  it("rejects a corpus with an article silently removed", () => {
    const tampered = clone(FULL);
    tampered.articles = tampered.articles.slice(1);
    expect(() => verifyHelpCorpus(tampered)).toThrow(HelpCorpusDigestMismatchError);
  });

  it("has restricted content, so the filtering gates prove something", () => {
    expect(FULL.chunks.some((chunk) => chunk.visibility !== "public")).toBe(true);
    expect(FULL.sources.some((source) => source.visibility !== "public")).toBe(true);
  });
});

describe("what this bundle ships", () => {
  it("ships the public corpus, not the full one", () => {
    expect(HELP_PUBLIC_CORPUS).toBe(publicCorpusJson as unknown as HelpCorpus);
    // `HELP_CORPUS` is the default a caller gets, and it is the public floor.
    expect(HELP_CORPUS).toBe(HELP_PUBLIC_CORPUS);
    expect(HELP_PUBLIC_CORPUS_DIGEST).toBe(PUBLIC.digest);
  });

  it("contains public records only", () => {
    expect(isPublicOnly(PUBLIC)).toBe(true);
    expect(PUBLIC.sources.length).toBeGreaterThan(0);
  });

  it("is self-consistent on its own terms", () => {
    expect(() => verifyHelpCorpus(PUBLIC)).not.toThrow();
  });

  it("is smaller than the full corpus, and honest about being different", () => {
    expect(PUBLIC.articles.length).toBeLessThan(FULL.articles.length);
    expect(PUBLIC.digest).not.toBe(FULL.digest);
  });

  it("preserves record digests so a citation still verifies against the full corpus", () => {
    for (const source of PUBLIC.sources) {
      expect(source.digest).toBe(findSource(FULL, source.id)?.digest);
    }
    for (const chunk of PUBLIC.chunks) {
      expect(chunk.digest).toBe(FULL.chunks.find((c) => c.id === chunk.id)?.digest);
    }
  });

  it("does not carry restricted text behind a filtered index", () => {
    // The restricted bytes must be absent, not merely unlisted.
    const serialized = JSON.stringify(PUBLIC);
    const restricted = FULL.chunks.filter((chunk) => chunk.visibility !== "public");
    expect(restricted.length).toBeGreaterThan(0);
    for (const chunk of restricted) {
      expect(serialized).not.toContain(chunk.text);
    }
  });

  it("resolves lookups against the public corpus only", () => {
    const gated = FULL.articles.find((article) => article.visibility !== "public");
    expect(gated).toBeDefined();
    expect(getHelpArticle(gated!.id)).toBeUndefined();

    const publicArticle = PUBLIC.articles[0];
    expect(getHelpArticle(publicArticle.id)).toBeDefined();
    expect(chunksForPublicArticle(publicArticle.id).length).toBeGreaterThan(0);
    expect(getHelpChunk(PUBLIC.chunks[0].id)).toBeDefined();
    expect(getHelpSource(PUBLIC.sources[0].id)).toBeDefined();
  });
});
