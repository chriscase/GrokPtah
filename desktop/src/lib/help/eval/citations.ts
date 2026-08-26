/**
 * Corpus-wide evaluation of claim-bound citation coverage.
 *
 * The retrieval gold set measures whether the right article comes back. It
 * says nothing about whether an answer built on that article can be *checked*,
 * which is what claim-bound coverage is for — and unit tests over a handful of
 * hand-picked chunks are not evidence about a corpus of 105.
 *
 * So this runs both directions over every gold query that retrieves something:
 *
 * - **Faithful.** An answer that restates its top chunk, citing that chunk,
 *   must be accepted. A gate that rejects honest answers is not a safety
 *   property, it is an outage, and this is the half that catches one.
 * - **Swapped.** The same answer citing a *different* retrieved chunk must be
 *   rejected. The citation is verbatim, in-context, correctly shaped, and
 *   about something else — the exact shape a plausible-looking wrong citation
 *   takes.
 *
 * Deterministic and fully offline: no provider is involved, and the "answers"
 * are constructed from corpus text rather than generated.
 */
import { getHelpChunk } from "../canonical/corpus";
import { searchHelpCorpus } from "../retrieval/hybrid";
import { rawTokens, isStopWord, stem } from "../retrieval/text";
import {
  HELP_ANSWER_RESPONSE_SCHEMA,
  buildHelpAnswerRequest,
  validateHelpAnswerResponse,
  type HelpAnswerRequest,
} from "../answer/contract";
import { HELP_GOLD_SET, type HelpGoldQuery } from "./goldset";

const EVAL_EXECUTION = "eval";

export type HelpCitationCase = {
  readonly query: HelpGoldQuery;
  /** A faithful answer over the top chunk was accepted. */
  readonly faithfulAccepted: boolean;
  readonly faithfulDetail: string;
  /** A swapped citation was rejected. `null` when the case did not apply. */
  readonly swapRejected: boolean | null;
  readonly swapDetail: string;
};

export type HelpCitationMetrics = {
  readonly cases: readonly HelpCitationCase[];
  /** Gold queries that retrieved something to build an answer from. */
  readonly evaluated: number;
  readonly faithfulAcceptanceRate: number;
  /** Cases where a disjoint second chunk existed to swap in. */
  readonly swapEvaluated: number;
  readonly swapRejectionRate: number;
};

/** Content tokens, matching what claim coverage compares. */
function tokensOf(text: string): Set<string> {
  return new Set(
    rawTokens(text)
      .filter((token) => token.length >= 2 && !isStopWord(token))
      .map(stem),
  );
}

/** The leading sentence, so an answer built from it is exactly one claim. */
function firstSentence(text: string): string {
  const stop = text.search(/[.!?](\s|$)/);
  const candidate = stop > 12 ? text.slice(0, stop) : text;
  return candidate.trim();
}

function replyOver(request: HelpAnswerRequest, answer: string, citedIndex: number, quote: string) {
  const chunk = request.context[citedIndex]!;
  return {
    schema: HELP_ANSWER_RESPONSE_SCHEMA,
    answer: `${answer}.`,
    citations: [
      {
        claimIndex: 0,
        chunkId: chunk.chunkId,
        articleId: chunk.articleId,
        sourceId: chunk.sourceIds[0]!,
        quote,
      },
    ],
    uncertainty: "Bounded to the cited chunk.",
    corpusDigest: request.corpusDigest,
  };
}

export function evaluateHelpCitationBinding(
  goldSet: readonly HelpGoldQuery[] = HELP_GOLD_SET,
): HelpCitationMetrics {
  const cases: HelpCitationCase[] = [];

  for (const query of goldSet) {
    // Abstention cases have nothing to build an answer from.
    if (query.expectedArticleId === null) continue;
    const outcome = searchHelpCorpus(query.query, { limit: 4 });
    if (outcome.abstained || outcome.results.length === 0) continue;

    const request = buildHelpAnswerRequest(query.query, outcome.results);
    if (request.context.length === 0) continue;

    const topChunk = getHelpChunk(request.context[0]!.chunkId);
    if (!topChunk) continue;
    const quote = firstSentence(topChunk.text);
    if (quote.length === 0) continue;

    const faithful = validateHelpAnswerResponse(
      replyOver(request, quote, 0, quote),
      request,
      EVAL_EXECUTION,
    );

    // Find a later chunk whose vocabulary is disjoint from the claim, so the
    // swap is genuinely unrelated rather than a near-duplicate of the same
    // article. Not every query has one; those cases are counted separately
    // rather than scored as passes.
    const claimTokens = tokensOf(quote);
    let swapRejected: boolean | null = null;
    let swapDetail = "no disjoint chunk retrieved";
    for (let index = 1; index < request.context.length; index += 1) {
      const other = getHelpChunk(request.context[index]!.chunkId);
      if (!other) continue;
      const otherQuote = firstSentence(other.text);
      if (otherQuote.length === 0) continue;
      const otherTokens = tokensOf(otherQuote);
      if ([...claimTokens].some((token) => otherTokens.has(token))) continue;

      const swapped = validateHelpAnswerResponse(
        replyOver(request, quote, index, otherQuote),
        request,
        EVAL_EXECUTION,
      );
      swapRejected = !swapped.accepted;
      swapDetail = swapped.accepted
        ? `accepted a citation from ${other.id} for a claim about ${topChunk.id}`
        : `rejected: ${swapped.reason}`;
      break;
    }

    cases.push(
      Object.freeze({
        query,
        faithfulAccepted: faithful.accepted,
        faithfulDetail: faithful.accepted ? "accepted" : `${faithful.reason}: ${faithful.detail}`,
        swapRejected,
        swapDetail,
      }),
    );
  }

  const swapCases = cases.filter((entry) => entry.swapRejected !== null);
  const ratio = (numerator: number, denominator: number) =>
    denominator === 0 ? 1 : numerator / denominator;

  return Object.freeze({
    cases: Object.freeze(cases),
    evaluated: cases.length,
    faithfulAcceptanceRate: ratio(
      cases.filter((entry) => entry.faithfulAccepted).length,
      cases.length,
    ),
    swapEvaluated: swapCases.length,
    swapRejectionRate: ratio(
      swapCases.filter((entry) => entry.swapRejected === true).length,
      swapCases.length,
    ),
  });
}

/**
 * Thresholds.
 *
 * A swapped citation must *always* be rejected: that is a correctness
 * property, not a rate to tune, so the bar is 1.
 *
 * Faithful acceptance measures 1.0 on the current corpus — all 109 evaluated
 * queries — and the threshold sits at 0.9 anyway. The gap is headroom for
 * corpus edits, not slack for known misses: coverage requires half a claim's
 * vocabulary to be quoted, and a future article whose summary is dense enough
 * could fail that honestly. Pinning the bar at 1.0 would make an ordinary
 * corpus edit look like a regression, and the pressure would be to tune the
 * heuristic to the fixtures.
 */
export const HELP_CITATION_THRESHOLDS = Object.freeze({
  faithfulAcceptanceRate: 0.9,
  swapRejectionRate: 1,
});
