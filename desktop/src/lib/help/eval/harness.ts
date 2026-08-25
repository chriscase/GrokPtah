/**
 * Deterministic offline retrieval evaluation.
 *
 * Shared by the vitest regression gate and the CLI reporter so a number quoted
 * in a report and a number enforced in CI cannot disagree.
 */
import { searchHelpCorpus, type HelpRetrievalOptions } from "../retrieval/hybrid";
import { HELP_GOLD_SET, type HelpGoldCategory, type HelpGoldQuery } from "./goldset";

export type HelpEvalOutcome = {
  readonly query: HelpGoldQuery;
  readonly abstained: boolean;
  readonly confidence: number;
  readonly topArticleId: string | null;
  readonly rankOfRelevant: number | null;
  /** Top-1 result is relevant (standard multi-relevant Recall@1). */
  readonly hitAt1: boolean;
  /** Top-1 result is the single preferred article (stricter). */
  readonly exactTop: boolean;
  readonly hitAt3: boolean;
  readonly reciprocalRank: number;
  readonly citationOk: boolean;
  readonly citationDetail: string;
  /** A must-abstain query that returned results anyway. */
  readonly falseAnswer: boolean;
  /** An answerable query the retriever declined. */
  readonly missedAnswerable: boolean;
};

export type HelpEvalMetrics = {
  readonly total: number;
  readonly answerable: number;
  readonly mustAbstain: number;
  readonly recallAt1: number;
  /** Share of answerable queries whose top-1 is the preferred article. */
  readonly topExactRate: number;
  readonly recallAt3: number;
  readonly mrr: number;
  readonly citationAccuracy: number;
  readonly falseAnswerRate: number;
  readonly abstentionRecall: number;
  readonly answerableAbstentionRate: number;
  readonly perCategory: Readonly<Record<string, { count: number; recallAt1: number; recallAt3: number }>>;
  readonly outcomes: readonly HelpEvalOutcome[];
};

/** `note: "cites docs/X.md#Heading"` becomes an exact citation assertion. */
function expectedAnchor(entry: HelpGoldQuery): string | null {
  const match = entry.note?.match(/cites\s+(\S+\.md#.+)$/);
  return match ? match[1]!.trim() : null;
}

function relevantSet(entry: HelpGoldQuery): Set<string> {
  const relevant = new Set<string>();
  if (entry.expectedArticleId) relevant.add(entry.expectedArticleId);
  for (const articleId of entry.alsoRelevant ?? []) relevant.add(articleId);
  return relevant;
}

export function evaluateHelpQuery(
  entry: HelpGoldQuery,
  options: HelpRetrievalOptions = {},
): HelpEvalOutcome {
  // Restricted content must be reachable for evaluation; access filtering is
  // a separate, independently tested concern.
  const outcome = searchHelpCorpus(entry.query, { limit: 5, ...options });
  const relevant = relevantSet(entry);
  const results = outcome.results;
  const topArticleId = results[0]?.articleId ?? null;

  let rankOfRelevant: number | null = null;
  for (const [index, result] of results.entries()) {
    if (relevant.has(result.articleId)) {
      rankOfRelevant = index + 1;
      break;
    }
  }

  // Recall@1 in the standard multi-relevant sense: is the top result relevant.
  // Several queries are genuinely answered by either of two articles (a
  // question about idempotency is covered by both durable recovery and the
  // prompt queue), so crediting only one id would understate ranking quality.
  // `exactTop` keeps the stricter view alongside it.
  const hitAt1 = topArticleId !== null && relevant.has(topArticleId);
  const exactTop = entry.expectedArticleId !== null && topArticleId === entry.expectedArticleId;
  const hitAt3 = rankOfRelevant !== null && rankOfRelevant <= 3;
  const reciprocalRank = rankOfRelevant === null ? 0 : 1 / rankOfRelevant;

  // Citation correctness: every citation on the rank-1 result must resolve,
  // and where the gold entry names an exact anchor it must be present.
  let citationOk = true;
  let citationDetail = "n/a";
  if (entry.expectedArticleId !== null && !outcome.abstained && results[0]) {
    const citations = results[0].citations;
    const anchors = citations.map((citation) => `${citation.path}#${citation.heading}`);
    if (citations.length === 0) {
      citationOk = false;
      citationDetail = "no citations on rank-1 result";
    } else if (citations.some((citation) => citation.articleId !== results[0]!.articleId)) {
      citationOk = false;
      citationDetail = "citation does not belong to the cited article";
    } else {
      const required = expectedAnchor(entry);
      if (required && !anchors.includes(required)) {
        citationOk = exactTop ? false : true;
        citationDetail = exactTop
          ? `expected anchor ${required}, got ${anchors.join(", ")}`
          : `not evaluated (rank-1 article was ${topArticleId})`;
      } else {
        citationDetail = anchors.join(", ");
      }
    }
  }

  return {
    query: entry,
    abstained: outcome.abstained,
    confidence: outcome.confidence,
    topArticleId,
    rankOfRelevant,
    hitAt1,
    exactTop,
    hitAt3,
    reciprocalRank,
    citationOk,
    citationDetail,
    falseAnswer: entry.expectedArticleId === null && !outcome.abstained,
    missedAnswerable: entry.expectedArticleId !== null && outcome.abstained,
  };
}

export function evaluateHelpGoldSet(
  entries: readonly HelpGoldQuery[] = HELP_GOLD_SET,
  options: HelpRetrievalOptions = {},
): HelpEvalMetrics {
  const outcomes = entries.map((entry) => evaluateHelpQuery(entry, options));
  const answerable = outcomes.filter((outcome) => outcome.query.expectedArticleId !== null);
  const mustAbstain = outcomes.filter((outcome) => outcome.query.expectedArticleId === null);

  const ratio = (numerator: number, denominator: number) => (denominator === 0 ? 1 : numerator / denominator);

  const perCategory: Record<string, { count: number; recallAt1: number; recallAt3: number }> = {};
  const categories = new Set<HelpGoldCategory>(entries.map((entry) => entry.category));
  for (const category of categories) {
    const slice = answerable.filter((outcome) => outcome.query.category === category);
    perCategory[category] = {
      count: slice.length,
      recallAt1: ratio(slice.filter((outcome) => outcome.hitAt1).length, slice.length),
      recallAt3: ratio(slice.filter((outcome) => outcome.hitAt3).length, slice.length),
    };
  }

  const cited = answerable.filter((outcome) => !outcome.abstained);

  return {
    total: outcomes.length,
    answerable: answerable.length,
    mustAbstain: mustAbstain.length,
    recallAt1: ratio(answerable.filter((outcome) => outcome.hitAt1).length, answerable.length),
    topExactRate: ratio(answerable.filter((outcome) => outcome.exactTop).length, answerable.length),
    recallAt3: ratio(answerable.filter((outcome) => outcome.hitAt3).length, answerable.length),
    mrr: ratio(
      answerable.reduce((total, outcome) => total + outcome.reciprocalRank, 0),
      answerable.length,
    ),
    citationAccuracy: ratio(cited.filter((outcome) => outcome.citationOk).length, cited.length),
    falseAnswerRate: ratio(mustAbstain.filter((outcome) => outcome.falseAnswer).length, mustAbstain.length),
    abstentionRecall: ratio(mustAbstain.filter((outcome) => !outcome.falseAnswer).length, mustAbstain.length),
    answerableAbstentionRate: ratio(
      answerable.filter((outcome) => outcome.missedAnswerable).length,
      answerable.length,
    ),
    perCategory: Object.freeze(perCategory),
    outcomes: Object.freeze(outcomes),
  };
}

/**
 * Thresholds the regression gate enforces.
 *
 * Set below current measured performance with deliberate headroom so ordinary
 * corpus edits do not fail the build, while a real regression does.
 */
export const HELP_EVAL_THRESHOLDS = Object.freeze({
  recallAt1: 0.8,
  topExactRate: 0.74,
  recallAt3: 0.88,
  mrr: 0.84,
  citationAccuracy: 1,
  falseAnswerRate: 0.06,
  answerableAbstentionRate: 0.12,
});
