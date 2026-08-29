/**
 * Pin the TypeScript digest implementation to the Rust one.
 *
 * `generated/digest-parity.json` is emitted by `help-codegen` from
 * `crates/common/grokptah-help-contract/src/digest.rs`. If either
 * implementation changes, these fail. The vectors deliberately cover the
 * places a naive port diverges.
 */

import { describe, expect, it } from "vitest";

import parity from "./generated/digest-parity.json";
import { domainDigest, lengthPrefixed, sha256Hex } from "./canonical/digest";

type ParityFile = {
  sha256: Array<{ input: string; sha256: string }>;
  domainDigests: Array<{
    domain: string;
    fields: string[];
    lengthPrefixed: string;
    digest: string;
  }>;
  articleDigests: Array<{
    name: string;
    id: string;
    title: string;
    topic: string;
    summary: string;
    body: string;
    visibility: string;
    aliases: string[];
    keywords: string[];
    capabilityIds: string[];
    sourceDigests: string[];
    digest: string;
  }>;
  requestDigests: Array<{
    name: string;
    requestId: string;
    corpusDigest: string;
    manifestRevision: number;
    question: string;
    locale: string;
    context: Array<{
      chunk_id: string;
      chunk_digest: string;
      source_ids: string[];
      text: string;
    }>;
    instruction: string;
    digest: string;
  }>;
};

const vectors = parity as unknown as ParityFile;

describe("digest parity with the Rust implementation", () => {
  it("reproduces every sha256 vector", () => {
    expect(vectors.sha256.length).toBeGreaterThan(0);
    for (const vector of vectors.sha256) {
      expect(sha256Hex(vector.input)).toBe(vector.sha256);
    }
  });

  it("reproduces every length-prefixed encoding", () => {
    expect(vectors.domainDigests.length).toBeGreaterThan(0);
    for (const vector of vectors.domainDigests) {
      expect(lengthPrefixed(vector.fields)).toBe(vector.lengthPrefixed);
    }
  });

  it("reproduces every domain digest", () => {
    for (const vector of vectors.domainDigests) {
      expect(domainDigest(vector.domain, vector.fields)).toBe(vector.digest);
    }
  });

  it("counts UTF-8 bytes, not characters", () => {
    // A one-character, four-byte field. Using String.length here would emit
    // "1:" and silently disagree with Rust on every non-ASCII field.
    expect(lengthPrefixed(["\u{1F600}"])).toBe("4:\u{1F600}");
    expect(lengthPrefixed(["café"])).toBe("5:café");
  });

  it("is injective where a separator scheme is not", () => {
    // Both would join to "a|b" under a naive scheme.
    expect(lengthPrefixed(["a|b"])).not.toBe(lengthPrefixed(["a", "b"]));
  });

  it("keeps identical field lists apart under different domains", () => {
    expect(domainDigest("grokptah.help.chunk.v1", ["same"])).not.toBe(
      domainDigest("grokptah.help.source.v1", ["same"]),
    );
  });

  /**
   * Reproduce the article digest the way `canonical/verify.ts` does.
   *
   * Kept inline rather than imported so the vectors pin the *encoding* — the
   * labelled, counted regions — and not merely whatever `verifyArticle`
   * currently happens to do.
   */
  const region = (label: string, items: readonly string[]): string[] => [
    label,
    String(items.length),
    ...items,
  ];
  const articleDigest = (vector: ParityFile["articleDigests"][number]): string =>
    domainDigest("grokptah.help.article.v1", [
      vector.id,
      vector.title,
      vector.topic,
      vector.summary,
      vector.body,
      vector.visibility,
      ...region("aliases", vector.aliases),
      ...region("keywords", vector.keywords),
      ...region("capabilities", vector.capabilityIds),
      ...region("sources", vector.sourceDigests),
    ]);

  it("reproduces every article digest Rust emitted", () => {
    expect(vectors.articleDigests.length).toBeGreaterThan(0);
    for (const vector of vectors.articleDigests) {
      expect(articleDigest(vector), vector.name).toBe(vector.digest);
    }
  });

  it("gives every metadata mutation a digest of its own", () => {
    // Repartition, reorder, omission and duplication must each be visible.
    // Under the previous flat encoding `repartition-*` collided with `base`,
    // which is how a capability could be moved into an alias and still verify.
    const byName = new Map(vectors.articleDigests.map((v) => [v.name, v.digest]));
    const base = byName.get("base");
    expect(base).toBeDefined();

    for (const name of [
      "repartition-capability-into-aliases",
      "repartition-capability-into-keywords",
      "reorder-within-a-list",
      "omit-a-capability",
      "duplicate-a-capability",
    ]) {
      expect(byName.get(name), name).toBeDefined();
      expect(byName.get(name), `${name} must not collide with base`).not.toBe(base);
    }

    // Empty is not absent: a list holding one empty string is a different list.
    expect(byName.get("empty-lists")).not.toBe(byName.get("one-empty-string-alias"));

    // And no two vectors collide with each other.
    const digests = vectors.articleDigests.map((v) => v.digest);
    expect(new Set(digests).size).toBe(digests.length);
  });

  const requestDigest = (vector: ParityFile["requestDigests"][number]): string =>
    domainDigest("grokptah.help.request.v1", [
      vector.requestId,
      vector.corpusDigest,
      String(vector.manifestRevision),
      vector.question,
      vector.locale,
      vector.instruction,
      "context",
      String(vector.context.length),
      ...vector.context.flatMap((chunk) => [
        chunk.chunk_id,
        chunk.chunk_digest,
        chunk.text,
        "sources",
        String(chunk.source_ids.length),
        ...chunk.source_ids,
      ]),
    ]);

  it("binds every request context source id exactly as Rust does", () => {
    expect(vectors.requestDigests.length).toBeGreaterThan(1);
    for (const vector of vectors.requestDigests) {
      expect(requestDigest(vector), vector.name).toBe(vector.digest);
    }
    expect(vectors.requestDigests[0].digest).not.toBe(vectors.requestDigests[1].digest);
  });
});
