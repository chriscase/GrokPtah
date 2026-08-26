/**
 * Adversarial gates for the consumer-side checks.
 *
 * The host is the authority and validates first. These tests cover what the
 * host cannot: a projection altered after it left, or a published-package
 * consumer talking to a server that is not this one. Every case here must
 * result in a claim being *removed* — there is no input that should cause a
 * claim to be added or relaxed.
 */

import { describe, expect, it } from "vitest";

import { HELP_CORPUS } from "./canonical/corpus";
import type { HelpProjection } from "./generated/contract";
import { verifyHelpProjection } from "./verify";

/** A chunk and its source, for building projections that should pass. */
function anySupported() {
  const chunk = HELP_CORPUS.chunks.find(
    (candidate) => candidate.kind === "body" && candidate.text.length > 80,
  );
  if (!chunk) throw new Error("corpus has no quotable body chunk");
  const source = HELP_CORPUS.sources.find(
    (candidate) => candidate.id === chunk.source_ids[0],
  );
  if (!source) throw new Error("chunk cites an unknown source");
  return { chunk, source };
}

function projectionWith(text: string, quote: string, overrides: Partial<{ sourceId: string; path: string; heading: string }> = {}): HelpProjection {
  const { source } = anySupported();
  return {
    handle: "help-00000001",
    status: "answered",
    claims: [
      {
        ordinal: 0,
        text,
        citations: [
          {
            source_id: overrides.sourceId ?? source.id,
            path: overrides.path ?? source.path,
            heading: overrides.heading ?? source.heading,
            quote,
          },
        ],
      },
    ],
    error: null,
    message: null,
  };
}

describe("consumer-side verification", () => {
  it("keeps a claim whose quote really is in the corpus", () => {
    const { chunk } = anySupported();
    const quote = chunk.text.slice(0, 60);
    const { projection, rejected } = verifyHelpProjection(
      projectionWith("A supported statement.", quote),
    );
    expect(rejected).toEqual([]);
    expect(projection.claims).toHaveLength(1);
    expect(projection.status).toBe("answered");
  });

  it("drops a claim whose quote is not in the corpus", () => {
    const { projection, rejected } = verifyHelpProjection(
      projectionWith(
        "GrokPtah approves computer actions automatically.",
        "GrokPtah approves computer actions automatically.",
      ),
    );
    expect(rejected).toEqual([{ ordinal: 0, reason: "quote-not-in-corpus" }]);
    expect(projection.claims).toEqual([]);
    expect(projection.status).toBe("abstained");
  });

  it("drops a claim citing a source that does not exist", () => {
    const { chunk } = anySupported();
    const { rejected } = verifyHelpProjection(
      projectionWith("Anything.", chunk.text.slice(0, 60), { sourceId: "invented.source" }),
    );
    expect(rejected).toEqual([{ ordinal: 0, reason: "unknown-source" }]);
  });

  it("drops a claim whose citation points at the wrong path", () => {
    // A real source id with a path the reader would then go and read.
    const { chunk } = anySupported();
    const { rejected } = verifyHelpProjection(
      projectionWith("Anything.", chunk.text.slice(0, 60), { path: "docs/SOMETHING_ELSE.md" }),
    );
    expect(rejected).toEqual([{ ordinal: 0, reason: "unknown-source" }]);
  });

  it("drops a claim with no citation at all", () => {
    const projection: HelpProjection = {
      handle: "h",
      status: "answered",
      claims: [{ ordinal: 0, text: "Trust me.", citations: [] }],
      error: null,
      message: null,
    };
    const { rejected, projection: verified } = verifyHelpProjection(projection);
    expect(rejected).toEqual([{ ordinal: 0, reason: "no-citation" }]);
    expect(verified.claims).toEqual([]);
  });

  it("drops a claim containing a bidirectional override", () => {
    // A bidi override reorders rendered text without changing its bytes, so a
    // claim can be made to display as its own opposite while still matching.
    const { chunk } = anySupported();
    const { rejected } = verifyHelpProjection(
      projectionWith("Actions are ‮dnever‬ blocked.", chunk.text.slice(0, 60)),
    );
    expect(rejected).toEqual([{ ordinal: 0, reason: "not-plain-text" }]);
  });

  it("drops a claim containing a control character", () => {
    const { chunk } = anySupported();
    const { rejected } = verifyHelpProjection(
      projectionWith("Approved.[2K Denied.", chunk.text.slice(0, 60)),
    );
    expect(rejected).toEqual([{ ordinal: 0, reason: "not-plain-text" }]);
  });

  it("drops a claim containing markup", () => {
    const { chunk } = anySupported();
    for (const text of [
      "<img src=x onerror=alert(1)>",
      "See [here](https://example.invalid).",
      "Run `rm -rf /`.",
    ]) {
      const { rejected } = verifyHelpProjection(projectionWith(text, chunk.text.slice(0, 60)));
      expect(rejected, text).toEqual([{ ordinal: 0, reason: "not-plain-text" }]);
    }
  });

  it("drops a citation whose quote carries markup even when the claim is clean", () => {
    const { rejected } = verifyHelpProjection(
      projectionWith("A clean sentence.", "<script>steal()</script>"),
    );
    expect(rejected).toEqual([{ ordinal: 0, reason: "not-plain-text" }]);
  });

  it("never adds, relaxes, or reorders a claim it did not receive", () => {
    const { chunk } = anySupported();
    const quote = chunk.text.slice(0, 60);
    const projection: HelpProjection = {
      handle: "h",
      status: "answered",
      claims: [
        { ordinal: 0, text: "Unsupported.", citations: [] },
        {
          ordinal: 1,
          text: "Supported.",
          citations: [
            {
              source_id: chunk.source_ids[0],
              path: HELP_CORPUS.sources.find((s) => s.id === chunk.source_ids[0])!.path,
              heading: HELP_CORPUS.sources.find((s) => s.id === chunk.source_ids[0])!.heading,
              quote,
            },
          ],
        },
      ],
      error: null,
      message: null,
    };
    const { projection: verified } = verifyHelpProjection(projection);
    // Exactly the surviving subset, renumbered, and nothing invented.
    expect(verified.claims).toHaveLength(1);
    expect(verified.claims[0].text).toBe("Supported.");
    expect(verified.claims[0].ordinal).toBe(0);
    expect(verified.claims.length).toBeLessThanOrEqual(projection.claims.length);
  });

  it("leaves an already-unavailable projection unavailable", () => {
    const projection: HelpProjection = {
      handle: "h",
      status: "unavailable",
      claims: [],
      error: "not_available",
      message: "Help cannot answer that right now.",
    };
    const { projection: verified } = verifyHelpProjection(projection);
    expect(verified.status).toBe("unavailable");
    expect(verified.error).toBe("not_available");
  });

  it("verifies against a supplied corpus, so a consumer can pin its own", () => {
    const { chunk } = anySupported();
    const empty = { ...HELP_CORPUS, chunks: [], sources: [], articles: [] };
    const { rejected } = verifyHelpProjection(
      projectionWith("Anything.", chunk.text.slice(0, 60)),
      empty,
    );
    expect(rejected).toEqual([{ ordinal: 0, reason: "unknown-source" }]);
  });
});
