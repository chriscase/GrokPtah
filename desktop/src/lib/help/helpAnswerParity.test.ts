import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import {
  helpAnswerRequestDigest,
  type HelpAnswerRequestCore,
} from "./answer/contract";

/**
 * The same fixture the Rust answer crate executes.
 *
 * The admission binds the request digest. If the two implementations
 * disagreed about how a request is digested, every admission would fail to
 * verify — or one minted for a different body would verify anyway. This suite
 * fails rather than letting that drift go unnoticed.
 *
 * Resolved from this file rather than the working directory, so the gate does
 * not silently depend on where the runner was invoked from.
 */
const ANSWER_CRATE = resolve(
  dirname(fileURLToPath(import.meta.url)),
  "..", "..", "..", "..",
  "crates", "common", "grokptah-help-answer",
);

const FIXTURE = JSON.parse(
  readFileSync(resolve(ANSWER_CRATE, "fixtures", "request-digest-parity.json"), "utf8"),
) as {
  cases: Array<{ name: string; core: HelpAnswerRequestCore }>;
};

/**
 * Digests recorded from the Rust implementation.
 *
 * Written down rather than recomputed so a change on either side has to be
 * made deliberately on both. `cargo test -p grokptah-help-answer parity --
 * --nocapture` prints these.
 */
const RUST_DIGESTS: Record<string, string> = {
  "empty context":
    "sha256:7ea706cc805b8181e222107ef3c1871abaf7527d9b1fa94124c4651efdea5e67",
  "one chunk":
    "sha256:bef9a6d385e553218c67532f94d1ccee9d616d4dd8728afb65b277727d1a0d6d",
  "source ids out of order digest the same as sorted":
    "sha256:7baaa6c7fa16255bdf67b009922fd63fffbd22b9de6dca5a9df8acdb52d1808d",
  "multi-byte and astral text":
    "sha256:1f897639c8b6a277d17a90e14ece7c2564dd130df5dfc6f437f0aa3e9469fc41",
  "two chunks, order significant":
    "sha256:866d0ff67163c4331c4f4b744ca0ae144bb4fc6834f6e7b69ad5c9937a563bde",
};

describe("answer request digest parity", () => {
  it("covers every fixture case", () => {
    expect(FIXTURE.cases.length).toBeGreaterThan(0);
    expect(FIXTURE.cases.map((entry) => entry.name).sort()).toEqual(
      Object.keys(RUST_DIGESTS).sort(),
    );
  });

  it.each(FIXTURE.cases.map((entry) => [entry.name, entry.core] as const))(
    "agrees with the Rust implementation on %s",
    (name, core) => {
      expect(helpAnswerRequestDigest(core)).toBe(RUST_DIGESTS[name]);
    },
  );

  it("does not collide across distinct requests", () => {
    const digests = FIXTURE.cases.map((entry) => helpAnswerRequestDigest(entry.core));
    expect(new Set(digests).size).toBe(digests.length);
  });

  it("is insensitive to source id order but not to membership", () => {
    const base = FIXTURE.cases.find((entry) => entry.core.context.length > 0);
    expect(base).toBeDefined();
    const core = base!.core;
    const chunk = core.context[0]!;

    const reordered: HelpAnswerRequestCore = {
      ...core,
      context: [{ ...chunk, sourceIds: [...chunk.sourceIds].reverse() }],
    };
    expect(helpAnswerRequestDigest(reordered)).toBe(helpAnswerRequestDigest(core));

    const extra: HelpAnswerRequestCore = {
      ...core,
      context: [{ ...chunk, sourceIds: [...chunk.sourceIds, "added.anchor"] }],
    };
    expect(helpAnswerRequestDigest(extra)).not.toBe(helpAnswerRequestDigest(core));
  });
});
