import { describe, expect, it } from "vitest";
import { HELP_EVAL_THRESHOLDS, evaluateHelpGoldSet } from "./eval/harness";
import { HELP_GOLD_ANSWERABLE, HELP_GOLD_MUST_ABSTAIN, HELP_GOLD_SET } from "./eval/goldset";

/**
 * Retrieval regression gate.
 *
 * Deterministic and offline: no network, no sampling, no clock. The same
 * harness backs `scripts/run-help-eval.mjs`, so a number quoted in a report
 * and a number enforced here cannot disagree.
 */
describe("Help retrieval quality", () => {
  const metrics = evaluateHelpGoldSet();

  it("covers every required query class at meaningful size", () => {
    expect(HELP_GOLD_SET.length).toBeGreaterThanOrEqual(120);
    expect(new Set(HELP_GOLD_SET.map((entry) => entry.id)).size).toBe(HELP_GOLD_SET.length);
    for (const category of [
      "exact", "paraphrase", "expert", "misspelling",
      "multilingual", "adversarial", "secret", "unsupported",
    ]) {
      expect(
        HELP_GOLD_SET.filter((entry) => entry.category === category).length,
        category,
      ).toBeGreaterThan(0);
    }
    expect(HELP_GOLD_ANSWERABLE.length).toBeGreaterThan(100);
    expect(HELP_GOLD_MUST_ABSTAIN.length).toBeGreaterThanOrEqual(20);
  });

  it("meets the Recall@1 threshold", () => {
    expect(metrics.recallAt1).toBeGreaterThanOrEqual(HELP_EVAL_THRESHOLDS.recallAt1);
  });

  it("meets the exact-article top-1 threshold", () => {
    expect(metrics.topExactRate).toBeGreaterThanOrEqual(HELP_EVAL_THRESHOLDS.topExactRate);
  });

  it("meets the Recall@3 threshold", () => {
    expect(metrics.recallAt3).toBeGreaterThanOrEqual(HELP_EVAL_THRESHOLDS.recallAt3);
  });

  it("meets the MRR threshold", () => {
    expect(metrics.mrr).toBeGreaterThanOrEqual(HELP_EVAL_THRESHOLDS.mrr);
  });

  it("cites correctly on every answered query", () => {
    expect(metrics.citationAccuracy).toBeGreaterThanOrEqual(HELP_EVAL_THRESHOLDS.citationAccuracy);
    const bad = metrics.outcomes.filter(
      (outcome) => outcome.query.expectedArticleId !== null && !outcome.abstained && !outcome.citationOk,
    );
    expect(bad.map((outcome) => `${outcome.query.id}: ${outcome.citationDetail}`)).toEqual([]);
  });

  it("keeps the false-answer rate under the threshold", () => {
    expect(metrics.falseAnswerRate).toBeLessThanOrEqual(HELP_EVAL_THRESHOLDS.falseAnswerRate);
  });

  it("still answers the large majority of answerable queries", () => {
    expect(metrics.answerableAbstentionRate).toBeLessThanOrEqual(
      HELP_EVAL_THRESHOLDS.answerableAbstentionRate,
    );
  });

  it("resolves misspelled and multilingual queries without regression", () => {
    // These two classes are the reason the semantic layer exists; a drop here
    // means the embedding or the correction path broke, not just a ranking shift.
    expect(metrics.perCategory.misspelling?.recallAt3).toBe(1);
    expect(metrics.perCategory.multilingual?.recallAt3).toBe(1);
  });

  it("produces identical metrics on a repeat run", () => {
    const repeat = evaluateHelpGoldSet();
    expect(repeat.recallAt1).toBe(metrics.recallAt1);
    expect(repeat.mrr).toBe(metrics.mrr);
    expect(repeat.falseAnswerRate).toBe(metrics.falseAnswerRate);
    expect(repeat.outcomes.map((outcome) => outcome.topArticleId)).toEqual(
      metrics.outcomes.map((outcome) => outcome.topArticleId),
    );
  });
});
