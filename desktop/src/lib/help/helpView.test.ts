import { describe, expect, it } from "vitest";

import { HELP_PUBLIC_CORPUS } from "./canonical/corpus";
import {
  HELP_AMBIGUITY_RATIO,
  HELP_CLEAR_SCORE,
  HELP_MAX_QUERY_CHARS,
  HELP_VIEW_CONTRACT,
  HELP_VIEW_UNKNOWNS,
  helpViewState,
  rejectQuery,
} from "./view";
import {
  HELP_VIEW_EMPTY_CORPUS,
  HELP_VIEW_FIXTURE_CORPUS,
  HELP_VIEW_FIXTURE_QUERIES,
  HELP_VIEW_MISSING_SOURCE_CORPUS,
  corpusWithTamperedChunk,
} from "./view.fixtures";

const CORPUS = HELP_VIEW_FIXTURE_CORPUS;

describe("Help view states", () => {
  it("presents a decisive leader as the answer", () => {
    const view = helpViewState(HELP_VIEW_FIXTURE_QUERIES.answer, CORPUS);

    expect(view.contract).toBe(HELP_VIEW_CONTRACT);
    expect(view.status).toBe("answer");
    expect(view.answer?.articleId).toBe("fixture.lantern-workspace");
    expect(view.candidates.map((candidate) => candidate.articleId)).not.toContain(
      "fixture.lantern-workspace",
    );
  });

  it("refuses to pick between two articles it cannot tell apart", () => {
    const view = helpViewState(HELP_VIEW_FIXTURE_QUERIES.ambiguous, CORPUS);

    expect(view.status).toBe("ambiguous");
    expect(view.answer).toBeNull();
    const [first, second] = view.candidates;
    expect(first.articleId).toBe("fixture.northern-relay");
    expect(second.articleId).toBe("fixture.southern-relay");
    // The tie is real, not an artefact of the wording of this test.
    expect(second.score / first.score).toBeGreaterThanOrEqual(HELP_AMBIGUITY_RATIO);
    expect(first.score).toBeLessThan(HELP_CLEAR_SCORE);
  });

  it("says a weak match is weak rather than answering with it", () => {
    const view = helpViewState(HELP_VIEW_FIXTURE_QUERIES.lowConfidence, CORPUS);

    expect(view.status).toBe("low-confidence");
    expect(view.answer).toBeNull();
    expect(view.abstainReason).toBe("below-threshold");
  });

  it("separates 'nothing matched' from 'nothing matched well'", () => {
    const nothing = helpViewState(HELP_VIEW_FIXTURE_QUERIES.noMatch, CORPUS);
    const weak = helpViewState(HELP_VIEW_FIXTURE_QUERIES.lowConfidence, CORPUS);

    expect(nothing.status).toBe("no-match");
    expect(nothing.abstainReason).toBe("no-match");
    expect(weak.status).toBe("low-confidence");
    expect(nothing.headline).not.toBe(weak.headline);
  });

  it("treats an unasked question as browsing, not as a failure", () => {
    const view = helpViewState(HELP_VIEW_FIXTURE_QUERIES.browse, CORPUS);

    expect(view.status).toBe("browse");
    expect(view.abstainReason).toBe("no-query");
    expect(view.rejection).toBeNull();
  });

  it("reports an empty corpus as nothing documented", () => {
    const view = helpViewState("anything at all", HELP_VIEW_EMPTY_CORPUS);

    expect(view.status).toBe("no-match");
    expect(view.abstainReason).toBe("empty-corpus");
  });

  it("never carries an answer outside the answer status", () => {
    for (const query of Object.values(HELP_VIEW_FIXTURE_QUERIES)) {
      const view = helpViewState(query, CORPUS);
      if (view.status === "answer") expect(view.answer).not.toBeNull();
      else expect(view.answer).toBeNull();
    }
  });

  it("carries the retriever's own verdict beside the derived status", () => {
    const view = helpViewState(HELP_VIEW_FIXTURE_QUERIES.noMatch, CORPUS);

    expect(view.retrievalMode).toBe("offline-hybrid");
    expect(view.corpusDigest).toBe(CORPUS.digest);
    expect(view.abstainReason).toBe("no-match");
  });
});

describe("query bounds", () => {
  it("rejects an over-long query before it reaches the index", () => {
    const view = helpViewState("x".repeat(HELP_MAX_QUERY_CHARS + 1), CORPUS);

    expect(view.status).toBe("rejected");
    expect(view.rejection).toBe("query-too-long");
    // A rejection is not an abstention, and must not read as one.
    expect(view.abstainReason).toBeNull();
  });

  it("rejects a query carrying control characters", () => {
    const view = helpViewState("lantern\u0007workspace", CORPUS);

    expect(view.status).toBe("rejected");
    expect(view.rejection).toBe("control-characters");
  });

  it("rejects a query that is short in characters but large in bytes", () => {
    // Three-byte code points, one UTF-16 unit each: 400 characters is under
    // the 512-character ceiling and 1200 bytes is over the 1024-byte one, so
    // the byte bound is reachable rather than dead validation.
    const query = "\u6E2C".repeat(400);

    expect(query.length).toBeLessThanOrEqual(HELP_MAX_QUERY_CHARS);
    expect(new TextEncoder().encode(query).length).toBeGreaterThan(1_024);
    expect(rejectQuery(query)).toBe("query-too-many-bytes");
  });

  it("accepts an ordinary question", () => {
    expect(rejectQuery("how do I rotate a relay?")).toBeNull();
  });
});

describe("citation revalidation", () => {
  it("verifies every citation it shows", () => {
    const view = helpViewState(HELP_VIEW_FIXTURE_QUERIES.answer, CORPUS);
    const answer = view.answer;

    expect(answer).not.toBeNull();
    expect(answer!.citations.length).toBeGreaterThan(0);
    for (const citation of answer!.citations) {
      expect(citation.verified).toBe(true);
      expect(citation.path).toMatch(/^docs\//);
    }
    expect(answer!.unverifiedCitationCount).toBe(0);
  });

  it("quotes the corpus's current bytes rather than a remembered copy", () => {
    const chunkId = "fixture.lantern-workspace#body.0";
    const tampered = corpusWithTamperedChunk(chunkId);
    const view = helpViewState(HELP_VIEW_FIXTURE_QUERIES.answer, tampered);
    const shown = [view.answer, ...view.candidates].find(
      (candidate) => candidate?.chunkId === chunkId,
    );
    const inCorpus = tampered.chunks.find((chunk) => chunk.id === chunkId);

    expect(shown).toBeTruthy();
    // Byte-for-byte, from the corpus that was passed in — not from whatever
    // retrieval happened to carry along with its score.
    expect(shown!.quote).toBe(inCorpus!.text);
    expect(shown!.quote).toContain("altered after retrieval");
  });

  it("drops a citation the corpus cannot produce, and counts the drop", () => {
    const view = helpViewState(HELP_VIEW_FIXTURE_QUERIES.ambiguous, HELP_VIEW_MISSING_SOURCE_CORPUS);
    const candidate = view.candidates[0] ?? view.answer;

    expect(candidate).toBeTruthy();
    expect(candidate!.citations).toHaveLength(0);
    expect(candidate!.unverifiedCitationCount).toBeGreaterThan(0);
  });
});

describe("index caching", () => {
  it("does not serve one corpus's index to another that shares a digest", () => {
    // Regression: the index was cached by `corpus.digest`, so two different
    // documents carrying the same digest string were served one another's
    // index. A caller that builds its own corpus picks its own digest, so the
    // collision was reachable without anything being malformed.
    const chunkId = "fixture.lantern-workspace#body.0";
    const tampered = corpusWithTamperedChunk(chunkId);
    expect(tampered.digest).toBe(HELP_VIEW_FIXTURE_CORPUS.digest);

    // Warm the cache with the original, then search the altered one.
    helpViewState(HELP_VIEW_FIXTURE_QUERIES.answer, HELP_VIEW_FIXTURE_CORPUS);
    const view = helpViewState(HELP_VIEW_FIXTURE_QUERIES.answer, tampered);
    const shown = [view.answer, ...view.candidates].find(
      (candidate) => candidate?.chunkId === chunkId,
    );

    expect(shown?.quote).toContain("altered after retrieval");
  });
});

describe("unknowns", () => {
  it("claims no provider, model, cost, or latency", () => {
    expect(HELP_VIEW_UNKNOWNS.provider).toBe("unknown");
    expect(HELP_VIEW_UNKNOWNS.model).toBe("unknown");
    expect(HELP_VIEW_UNKNOWNS.cost).toBe("unknown");
    expect(HELP_VIEW_UNKNOWNS.latency).toBe("unknown");
    expect(HELP_VIEW_UNKNOWNS.note).toMatch(/No provider is contacted/);
  });
});

describe("the shipped corpus through the same contract", () => {
  it("answers a real question with verified citations", () => {
    // The fixtures deliberately avoid the shipped corpus; this one case proves
    // the contract is not fixture-shaped.
    const view = helpViewState("how do I approve a Computer Use action", HELP_PUBLIC_CORPUS);

    expect(["answer", "ambiguous", "low-confidence"]).toContain(view.status);
    if (view.status === "answer") {
      expect(view.answer?.citations.length).toBeGreaterThan(0);
      expect(view.answer?.unverifiedCitationCount).toBe(0);
    }
  });

  it("abstains on a question the documentation does not cover", () => {
    const view = helpViewState("what is the capital of Portugal", HELP_PUBLIC_CORPUS);

    expect(view.answer).toBeNull();
    expect(["low-confidence", "no-match"]).toContain(view.status);
  });
});
