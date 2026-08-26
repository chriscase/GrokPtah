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

// The gold set exercises retrieval over the *full* corpus, because that is
// what the host searches. The bundle's public subset is checked separately
// below. Test files are not bundled, so reading the private artifact here does
// not put it in anyone's package.
import fullCorpusJson from "./canonical/help-corpus.v1.json";
import { HELP_PUBLIC_CORPUS } from "./canonical/corpus";
import type { HelpCorpus } from "./generated/contract";

const CORPUS = fullCorpusJson as unknown as HelpCorpus;
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
      const outcome = searchHelpCorpus(probe.query, { corpus: CORPUS });
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
      const outcome = searchHelpCorpus(query, { corpus: CORPUS });
      if (outcome.kind === "results") {
        wrong.push(`${query} -> ${outcome.results[0]?.articleId} @ ${outcome.results[0]?.score.fused.toFixed(3)}`);
      }
    }
    expect(wrong, `answered an unanswerable question:\n${wrong.join("\n")}`).toEqual([]);
  });

  it("reports why it abstained", () => {
    expect(searchHelpCorpus("", { corpus: CORPUS }).kind).toBe("abstained");
    const empty = searchHelpCorpus("", { corpus: CORPUS });
    if (empty.kind === "abstained") expect(empty.reason).toBe("no-query");
    const unknown = searchHelpCorpus("capital of Portugal", { corpus: CORPUS });
    if (unknown.kind === "abstained") expect(unknown.reason).toBe("below-threshold");
  });

  it("is deterministic", () => {
    const first = searchHelpCorpus("recover an interrupted run", { corpus: CORPUS });
    const second = searchHelpCorpus("recover an interrupted run", { corpus: CORPUS });
    expect(JSON.stringify(first)).toBe(JSON.stringify(second));
  });

  it("binds results to the corpus it searched", () => {
    const outcome = searchHelpCorpus("recover an interrupted run", { corpus: CORPUS });
    expect(outcome.corpusDigest).toBe(CORPUS.digest);
    expect(outcome.mode).toBe("offline-hybrid");
  });

  it("returns the exact chunk bytes, not a paraphrase", () => {
    const outcome = searchHelpCorpus("recover an interrupted run", { corpus: CORPUS });
    expect(outcome.kind).toBe("results");
    if (outcome.kind !== "results") return;
    for (const result of outcome.results) {
      const chunk = CORPUS.chunks.find((candidate) => candidate.id === result.chunkId);
      expect(chunk).toBeDefined();
      expect(result.text).toBe(chunk!.text);
    }
  });

  it("shows at most one result per article", () => {
    const outcome = searchHelpCorpus("run", { corpus: CORPUS });
    if (outcome.kind !== "results") return;
    const ids = outcome.results.map((result) => result.articleId);
    expect(new Set(ids).size).toBe(ids.length);
  });

  it("honours the limit and its ceiling", () => {
    const outcome = searchHelpCorpus("run", { limit: 2, corpus: CORPUS });
    if (outcome.kind === "results") expect(outcome.results.length).toBeLessThanOrEqual(2);
    const huge = searchHelpCorpus("run", { limit: 10_000, corpus: CORPUS });
    if (huge.kind === "results") {
      expect(huge.results.length).toBeLessThanOrEqual(HELP_RETRIEVAL_MAX_LIMIT);
    }
  });

  it("filters by topic without leaking other topics", () => {
    const outcome = searchHelpCorpus("safety", { topic: "computer-use", corpus: CORPUS });
    if (outcome.kind !== "results") return;
    for (const result of outcome.results) {
      expect(result.topic).toBe("computer-use");
    }
  });

  it("keeps the abstention threshold above every negative's best score", () => {
    // Documents the calibration rather than asserting a magic number twice.
    for (const query of NEGATIVES) {
      expect(searchHelpCorpus(query, { corpus: CORPUS }).kind).toBe("abstained");
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
      searchHelpCorpus("recover an interrupted run", { corpus: CORPUS });
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

describe("retrieval over the bundled public corpus", () => {
  it("answers public questions from the bundle alone", () => {
    const outcome = searchHelpCorpus("recover an interrupted run", {
      corpus: HELP_PUBLIC_CORPUS,
    });
    expect(outcome.kind).toBe("results");
    if (outcome.kind !== "results") return;
    expect(outcome.results[0]?.articleId).toBe("operations.durable-recovery");
  });

  it("cannot surface a gated article, because it does not have one", () => {
    // Not "filters it out" — the bytes are not present. A renderer holding the
    // public bundle has nothing restricted to reveal even if it tried.
    const gated = CORPUS.articles.find((article) => article.visibility !== "public");
    expect(gated).toBeDefined();
    expect(HELP_PUBLIC_CORPUS.articles.some((a) => a.id === gated!.id)).toBe(false);

    const outcome = searchHelpCorpus(gated!.title, { corpus: HELP_PUBLIC_CORPUS });
    if (outcome.kind === "results") {
      expect(outcome.results.map((result) => result.articleId)).not.toContain(gated!.id);
    }
  });

  it("finds a gated article when the host supplies the wider corpus", () => {
    const gated = CORPUS.articles.find((article) => article.id === "operations.queue-and-steer");
    expect(gated).toBeDefined();
    const outcome = searchHelpCorpus("queue a prompt and steer a run", { corpus: CORPUS });
    expect(outcome.kind).toBe("results");
    if (outcome.kind !== "results") return;
    expect(outcome.results.map((result) => result.articleId)).toContain(gated!.id);
  });
});
