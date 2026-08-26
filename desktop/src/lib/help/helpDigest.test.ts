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
});
