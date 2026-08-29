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
import { HELP_DIGEST_DOMAINS, domainDigest } from "./canonical/digest";
import { HelpCorpusSchemaError, parseHelpCorpus } from "./canonical/schema";
import type { HelpChunk, HelpCorpus } from "./generated/contract";

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

  it("rejects duplicate source, article, and chunk ids before lookup", () => {
    for (const collection of ["sources", "articles", "chunks"] as const) {
      const tampered = clone(FULL);
      const records = [...tampered[collection], tampered[collection][0]];
      (tampered as unknown as Record<typeof collection, readonly unknown[]>)[collection] = records;
      expect(() => verifyHelpCorpus(tampered)).toThrow(HelpCorpusDigestMismatchError);
    }
  });

  it("rejects a restricted article shadowing a public id", () => {
    const tampered = clone(FULL);
    const publicArticle = tampered.articles.find((article) => article.visibility === "public")!;
    const restricted = tampered.articles.find((article) => article.visibility !== "public")!;
    (tampered as unknown as { articles: unknown[] }).articles.push({
      ...restricted,
      id: publicArticle.id,
    });
    expect(() => verifyHelpCorpus(tampered)).toThrow(HelpCorpusDigestMismatchError);
  });

  it("has restricted content, so the filtering gates prove something", () => {
    expect(FULL.chunks.some((chunk) => chunk.visibility !== "public")).toBe(true);
    expect(FULL.sources.some((source) => source.visibility !== "public")).toBe(true);
  });
});

describe("the bundled JSON runtime boundary", () => {
  it("accepts the generated public corpus without creating a second object", () => {
    expect(parseHelpCorpus(publicCorpusJson)).toBe(publicCorpusJson);
  });

  it("rejects unknown and missing fields", () => {
    const extra = JSON.parse(JSON.stringify(publicCorpusJson)) as Record<string, unknown>;
    extra.injected = "payload";
    expect(() => parseHelpCorpus(extra)).toThrow(HelpCorpusSchemaError);

    const missing = JSON.parse(JSON.stringify(publicCorpusJson)) as Record<string, unknown>;
    delete missing.digest;
    expect(() => parseHelpCorpus(missing)).toThrow(HelpCorpusSchemaError);
  });

  it("rejects invalid nested scalar and enum values", () => {
    const scalar = JSON.parse(JSON.stringify(publicCorpusJson)) as {
      chunks: Array<Record<string, unknown>>;
    };
    scalar.chunks[0].ordinal = "0";
    expect(() => parseHelpCorpus(scalar)).toThrow(HelpCorpusSchemaError);

    const enumeration = JSON.parse(JSON.stringify(publicCorpusJson)) as {
      articles: Array<Record<string, unknown>>;
    };
    enumeration.articles[0].visibility = "everyone";
    expect(() => parseHelpCorpus(enumeration)).toThrow(HelpCorpusSchemaError);
  });

  it("rejects an unknown schema version instead of interpreting it as v1", () => {
    const unknown = JSON.parse(JSON.stringify(publicCorpusJson)) as Record<string, unknown>;
    unknown.schema_version = "grokptah.help-canonical.v2";
    expect(() => parseHelpCorpus(unknown)).toThrow(HelpCorpusSchemaError);
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

/**
 * Re-mint a chunk's own digest after editing it.
 *
 * Without this a test that re-labels a chunk trips `verifyChunk`'s digest
 * check and passes for the wrong reason — it never reaches the rule it claims
 * to be about. Mutating away that rule then leaves the test green.
 */
function remintChunk(chunk: HelpChunk): void {
  chunk.digest = domainDigest(HELP_DIGEST_DOMAINS.chunk, [
    chunk.id,
    chunk.article_id,
    chunk.kind,
    String(chunk.ordinal),
    chunk.locale,
    chunk.text,
    chunk.visibility,
    ...chunk.source_ids,
  ]);
}

/**
 * Recompute the set-level digests, mirroring `Corpus::rebind_set_digests`.
 *
 * The region labels and counts are spelled out here rather than imported, so
 * this states the expected encoding independently of the implementation under
 * test.
 */
function rebindSetDigests(corpus: HelpCorpus): void {
  const mutable = corpus as unknown as { source_digest: string; digest: string };
  mutable.source_digest = domainDigest(
    HELP_DIGEST_DOMAINS.sourceSet,
    ["sources", String(corpus.sources.length), ...corpus.sources.map((source) => source.digest)],
  );
  mutable.digest = domainDigest(HELP_DIGEST_DOMAINS.corpus, [
    corpus.schema_version,
    corpus.content_version,
    "articles",
    String(corpus.articles.length),
    ...corpus.articles.map((article) => article.digest),
    "chunks",
    String(corpus.chunks.length),
    ...corpus.chunks.map((chunk) => chunk.digest),
    "sources",
    mutable.source_digest,
  ]);
}

/** The error `run` threw, or a failure if it threw nothing. */
function rejection(run: () => void): HelpCorpusDigestMismatchError {
  try {
    run();
  } catch (error) {
    expect(error).toBeInstanceOf(HelpCorpusDigestMismatchError);
    return error as HelpCorpusDigestMismatchError;
  }
  throw new Error("expected the corpus to be refused, but it verified");
}

describe("a tampered corpus is refused", () => {
  it("rejects a capability folded into aliases", () => {
    // The bypass this repair closes. The flat encoding hashed
    // `aliases ++ keywords ++ capability_ids`, so folding the lists together
    // left the article and corpus digests byte-identical while the capability
    // gate was gone.
    const tampered = clone(FULL);
    const gated = tampered.articles.find((article) => article.capability_ids.length > 0);
    expect(gated, "the corpus gates at least one article").toBeDefined();
    if (!gated) return;
    const mutable = gated as unknown as {
      aliases: string[];
      keywords: string[];
      capability_ids: string[];
    };
    mutable.aliases = [...gated.aliases, ...gated.keywords, ...gated.capability_ids];
    mutable.keywords = [];
    mutable.capability_ids = [];

    const error = rejection(() => verifyHelpCorpus(tampered));
    expect(error.record).toBe(`article:${gated.id}`);
  });

  it("rejects a single keyword moved into aliases", () => {
    const tampered = clone(FULL);
    const article = tampered.articles.find((entry) => entry.keywords.length > 0);
    expect(article).toBeDefined();
    if (!article) return;
    const mutable = article as unknown as { aliases: string[]; keywords: string[] };
    mutable.aliases = [...article.aliases, article.keywords[0]];
    mutable.keywords = article.keywords.slice(1);

    expect(rejection(() => verifyHelpCorpus(tampered)).record).toBe(`article:${article.id}`);
  });

  it("rejects a reordered alias list", () => {
    const tampered = clone(FULL);
    const article = tampered.articles.find((entry) => entry.aliases.length > 1);
    expect(article).toBeDefined();
    if (!article) return;
    (article as unknown as { aliases: string[] }).aliases = [...article.aliases].reverse();

    expect(rejection(() => verifyHelpCorpus(tampered)).record).toBe(`article:${article.id}`);
  });

  it("rejects a duplicated capability", () => {
    const tampered = clone(FULL);
    const gated = tampered.articles.find((article) => article.capability_ids.length > 0);
    expect(gated).toBeDefined();
    if (!gated) return;
    (gated as unknown as { capability_ids: string[] }).capability_ids = [
      ...gated.capability_ids,
      gated.capability_ids[0],
    ];

    expect(rejection(() => verifyHelpCorpus(tampered)).record).toBe(`article:${gated.id}`);
  });

  it("rejects a chunk more restricted than its article", () => {
    // Every other digest is re-minted, so the visibility relationship is the
    // only thing left wrong: this fails if that rule is removed.
    const tampered = clone(FULL);
    const publicArticle = tampered.articles.find((article) => article.visibility === "public");
    expect(publicArticle).toBeDefined();
    if (!publicArticle) return;
    const chunk = tampered.chunks.find((entry) => entry.article_id === publicArticle.id);
    expect(chunk).toBeDefined();
    if (!chunk) return;
    (chunk as unknown as { visibility: string }).visibility = "operator";
    remintChunk(chunk);
    rebindSetDigests(tampered);

    const error = rejection(() => verifyHelpCorpus(tampered));
    expect(error.record).toBe(`chunk:${chunk.id}`);
    expect(error.expected).toContain(`article ${publicArticle.id} is public`);
    expect(error.actual).toContain("chunk is operator");
  });

  it("rejects a chunk less restricted than its article", () => {
    const tampered = clone(FULL);
    const restricted = tampered.articles.find((article) => article.visibility === "operator");
    expect(restricted).toBeDefined();
    if (!restricted) return;
    const chunk = tampered.chunks.find((entry) => entry.article_id === restricted.id);
    expect(chunk).toBeDefined();
    if (!chunk) return;
    (chunk as unknown as { visibility: string }).visibility = "public";
    remintChunk(chunk);
    rebindSetDigests(tampered);

    const error = rejection(() => verifyHelpCorpus(tampered));
    expect(error.record).toBe(`chunk:${chunk.id}`);
    expect(error.actual).toContain("chunk is public");
  });

  it("rejects a chunk whose article is absent", () => {
    const tampered = clone(FULL);
    const orphan = tampered.chunks[0];
    tampered.articles = tampered.articles.filter((article) => article.id !== orphan.article_id);
    rebindSetDigests(tampered);

    const error = rejection(() => verifyHelpCorpus(tampered));
    expect(error.record).toBe(`chunk:${orphan.id}`);
    expect(error.actual).toBe("unknown article");
  });

  it("rejects a chunk with no sources after every digest is re-minted", () => {
    const tampered = clone(FULL);
    const chunk = tampered.chunks[0];
    (chunk as unknown as { source_ids: string[] }).source_ids = [];
    remintChunk(chunk);
    rebindSetDigests(tampered);
    expect(rejection(() => verifyHelpCorpus(tampered)).record).toBe(`chunk:${chunk.id}`);
  });

  it("rejects an unknown or substituted chunk source", () => {
    for (const replacement of ["unknown.source", FULL.sources.find((source) => source.visibility === "public")!.id]) {
      const tampered = clone(FULL);
      const chunk = tampered.chunks.find(
        (entry) => entry.visibility === "public" && !entry.source_ids.includes(replacement),
      );
      expect(chunk).toBeDefined();
      if (!chunk) continue;
      (chunk as unknown as { source_ids: string[] }).source_ids = [replacement];
      remintChunk(chunk);
      rebindSetDigests(tampered);
      expect(rejection(() => verifyHelpCorpus(tampered)).record).toBe(`chunk:${chunk.id}`);
    }
  });

  it("rejects a public chunk citing a restricted source", () => {
    const tampered = clone(FULL);
    const restricted = tampered.sources.find((source) => source.visibility === "operator")!;
    const chunk = tampered.chunks.find((entry) => entry.visibility === "public")!;
    (chunk as unknown as { source_ids: string[] }).source_ids = [restricted.id];
    remintChunk(chunk);
    rebindSetDigests(tampered);
    expect(rejection(() => verifyHelpCorpus(tampered)).record).toBe(`chunk:${chunk.id}`);
  });

  it("rejects a self-consistent unknown schema version", () => {
    const tampered = clone(FULL);
    (tampered as unknown as { schema_version: string }).schema_version =
      "grokptah.help-canonical.v2";
    rebindSetDigests(tampered);
    expect(rejection(() => verifyHelpCorpus(tampered)).record).toBe("schema-version");
  });

  it("rejects a dropped record even though every surviving digest is intact", () => {
    // The corpus digest counts its regions, so removing a chunk changes the
    // document without touching any record digest.
    const tampered = clone(FULL);
    tampered.chunks = tampered.chunks.slice(0, -1);

    expect(rejection(() => verifyHelpCorpus(tampered)).record).toBe("corpus");
  });
});
