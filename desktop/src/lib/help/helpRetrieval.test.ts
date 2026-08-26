/**
 * Retrieval gates, including a small gold set.
 *
 * The gold set is the only honest way to talk about a retriever. A ranking
 * function always returns *something*; whether that something is the right
 * article is a claim that needs evidence, and the negatives matter more than
 * the positives — a retriever that answers everything confidently is worse
 * than one that says it does not know.
 */

import { describe, expect, it } from "vitest";

import { HELP_CORPUS } from "./canonical/corpus";
import {
  HELP_ABSTENTION_THRESHOLD,
  HELP_RETRIEVAL_MAX_LIMIT,
  searchHelpCorpus,
} from "./retrieval/hybrid";
import { canonicalTerm, cosine, terms, trigrams, vectorize } from "./retrieval/text";

/** Questions a reader would actually type, and the article that answers them. */
const POSITIVES: Array<{ query: string; expect: string }> = [
  { query: "how do I recover an interrupted run", expect: "operations.durable-recovery" },
  { query: "restart duplicate send unknown", expect: "operations.durable-recovery" },
  { query: "resume after crash checkpoint", expect: "operations.durable-recovery" },
  { query: "what is the difference between builds and chats", expect: "getting-started.sessions" },
  { query: "find an earlier build", expect: "getting-started.search" },
  { query: "search my old sessions", expect: "getting-started.search" },
  { query: "provider route gateway policy", expect: "providers.routes" },
  { query: "grok bot versus grok build", expect: "providers.product-boundary" },
  { query: "embed grokptah in another product", expect: "providers.embedding" },
  { query: "computer use consent and boundaries", expect: "computer-use.boundaries" },
  { query: "can the agent click for me safely", expect: "computer-use.boundaries" },
  { query: "does a passing test mean certified", expect: "operations.evidence" },
  { query: "keyboard shortcuts and screen reader", expect: "getting-started.accessibility" },
  { query: "queue a prompt and steer a run", expect: "operations.queue-and-steer" },
  { query: "cited answer from help", expect: "getting-started.help-center" },
];

/** Questions this corpus genuinely cannot answer. It must say so. */
const NEGATIVES = [
  "what is the capital of Portugal",
  "recipe for sourdough starter hydration",
  "convert 40 celsius to fahrenheit",
  "who won the 1998 world cup final",
  "how do I file my taxes in Ireland",
];

describe("offline hybrid retrieval", () => {
  it("finds the right article for questions a reader would type", () => {
    const misses: string[] = [];
    for (const probe of POSITIVES) {
      const outcome = searchHelpCorpus(probe.query);
      if (outcome.kind !== "results") {
        misses.push(`${probe.query} -> abstained`);
        continue;
      }
      const top3 = outcome.results.slice(0, 3).map((result) => result.articleId);
      if (!top3.includes(probe.expect)) {
        misses.push(`${probe.query} -> ${top3.join(", ")} (wanted ${probe.expect})`);
      }
    }
    expect(misses, `gold-set misses:\n${misses.join("\n")}`).toEqual([]);
  });

  it("abstains rather than guessing at questions the corpus cannot answer", () => {
    const wrong: string[] = [];
    for (const query of NEGATIVES) {
      const outcome = searchHelpCorpus(query);
      if (outcome.kind === "results") {
        wrong.push(`${query} -> ${outcome.results[0]?.articleId} @ ${outcome.results[0]?.score.fused.toFixed(3)}`);
      }
    }
    expect(wrong, `answered an unanswerable question:\n${wrong.join("\n")}`).toEqual([]);
  });

  it("reports why it abstained", () => {
    expect(searchHelpCorpus("").kind).toBe("abstained");
    const empty = searchHelpCorpus("");
    if (empty.kind === "abstained") expect(empty.reason).toBe("no-query");
    const unknown = searchHelpCorpus("capital of Portugal");
    if (unknown.kind === "abstained") expect(unknown.reason).toBe("below-threshold");
  });

  it("is deterministic", () => {
    const first = searchHelpCorpus("recover an interrupted run");
    const second = searchHelpCorpus("recover an interrupted run");
    expect(JSON.stringify(first)).toBe(JSON.stringify(second));
  });

  it("binds results to the corpus it searched", () => {
    const outcome = searchHelpCorpus("recover an interrupted run");
    expect(outcome.corpusDigest).toBe(HELP_CORPUS.digest);
    expect(outcome.mode).toBe("offline-hybrid");
  });

  it("returns the exact chunk bytes, not a paraphrase", () => {
    const outcome = searchHelpCorpus("recover an interrupted run");
    expect(outcome.kind).toBe("results");
    if (outcome.kind !== "results") return;
    for (const result of outcome.results) {
      const chunk = HELP_CORPUS.chunks.find((candidate) => candidate.id === result.chunkId);
      expect(chunk).toBeDefined();
      expect(result.text).toBe(chunk!.text);
    }
  });

  it("shows at most one result per article", () => {
    const outcome = searchHelpCorpus("run");
    if (outcome.kind !== "results") return;
    const ids = outcome.results.map((result) => result.articleId);
    expect(new Set(ids).size).toBe(ids.length);
  });

  it("honours the limit and its ceiling", () => {
    const outcome = searchHelpCorpus("run", { limit: 2 });
    if (outcome.kind === "results") expect(outcome.results.length).toBeLessThanOrEqual(2);
    const huge = searchHelpCorpus("run", { limit: 10_000 });
    if (huge.kind === "results") {
      expect(huge.results.length).toBeLessThanOrEqual(HELP_RETRIEVAL_MAX_LIMIT);
    }
  });

  it("filters by topic without leaking other topics", () => {
    const outcome = searchHelpCorpus("safety", { topic: "computer-use" });
    if (outcome.kind !== "results") return;
    for (const result of outcome.results) {
      expect(result.topic).toBe("computer-use");
    }
  });

  it("keeps the abstention threshold above every negative's best score", () => {
    // Documents the calibration rather than asserting a magic number twice.
    for (const query of NEGATIVES) {
      const outcome = searchHelpCorpus(query);
      expect(outcome.kind).toBe("abstained");
    }
    expect(HELP_ABSTENTION_THRESHOLD).toBeGreaterThan(0);
    expect(HELP_ABSTENTION_THRESHOLD).toBeLessThan(1);
  });

  it("makes no network call", () => {
    // The module has no import that could make one; this asserts the observable
    // consequence rather than the absence.
    const originalFetch = globalThis.fetch;
    let called = false;
    globalThis.fetch = (() => {
      called = true;
      throw new Error("retrieval must not reach the network");
    }) as typeof fetch;
    try {
      searchHelpCorpus("recover an interrupted run");
    } finally {
      globalThis.fetch = originalFetch;
    }
    expect(called).toBe(false);
  });
});

describe("text normalisation", () => {
  it("folds accents and light plurals identically for index and query", () => {
    expect(canonicalTerm("Sessions")).toBe(canonicalTerm("session"));
    expect(canonicalTerm("café")).toBe(canonicalTerm("cafe"));
  });

  it("keeps identifiers intact", () => {
    expect(terms("sessionNewKind cursor_expired")).toContain("sessionnewkind");
  });

  it("drops stop words but keeps short identifiers", () => {
    expect(terms("what is the run")).toEqual(["run"]);
  });

  it("produces comparable trigram vectors", () => {
    const left = vectorize(trigrams("recover a durable run"));
    const right = vectorize(trigrams("recovering durable runs"));
    const unrelated = vectorize(trigrams("sourdough starter hydration"));
    expect(cosine(left, right)).toBeGreaterThan(cosine(left, unrelated));
  });
});
