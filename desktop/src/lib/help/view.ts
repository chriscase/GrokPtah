/**
 * What a Help surface is allowed to say about a retrieval.
 *
 * `retrieval/hybrid` decides what matched and when it does not know.
 * `canonical/verify` decides whether a citation is real. This module decides
 * how a *reader* is shown those decisions, and nothing else: it re-ranks
 * nothing, re-scores nothing, and cannot turn a decision into a stronger one
 * than the retriever made.
 *
 * It exists because the failure mode of a Help UI is not bad ranking — it is
 * the gap between "the retriever abstained" and what appears on screen. Three
 * rules close that gap here rather than in a component, so a redesign cannot
 * quietly drop them:
 *
 *   - **An abstention is never an answer.** `answer` is populated for exactly
 *     one status. Every other status carries candidates a reader may skim, and
 *     the surface must label them as such.
 *   - **A citation is re-resolved before it is shown.** Every quote is looked
 *     up in the corpus again and dropped if the bytes disagree, with the drop
 *     counted so a surface can disclose it instead of silently showing less.
 *   - **Unknowns stay unknown.** Nothing here observes a provider, a model, a
 *     price, or a latency, so all four are reported as unknown rather than
 *     defaulted to a value a reader would mistake for a measurement.
 *
 * Pure and synchronous: no clock, no randomness, no I/O, no Tauri. The desktop
 * component and an embedder render the same states from the same function.
 */

import type { HelpCorpus, HelpSourceAnchor, HelpTopic } from "./generated/contract";
import { findChunk, findSource } from "./canonical/verify";
import {
  HELP_ABSTENTION_THRESHOLD,
  searchHelpCorpus,
  type HelpRetrievalOptions,
  type HelpRetrievalOutcome,
  type HelpRetrievalResult,
} from "./retrieval/hybrid";

export const HELP_VIEW_CONTRACT = "grokptah.help-view.v1" as const;

/** Query bounds. Both are reachable: 512 UTF-16 units can exceed 1024 bytes. */
export const HELP_MAX_QUERY_CHARS = 512;
export const HELP_MAX_QUERY_BYTES = 1_024;

/**
 * At or above this fused score the leader is decisive, and a close runner-up
 * does not make the result ambiguous. Below it, a near-tie means the retriever
 * cannot distinguish two articles and the surface must not pick one.
 */
export const HELP_CLEAR_SCORE = 0.6;
/** A runner-up at least this close to an undecisive leader is a tie. */
export const HELP_AMBIGUITY_RATIO = 0.95;

/**
 * The one status a surface renders from.
 *
 * The retriever's own vocabulary is `results | abstained(reason)`, which does
 * not separate the cases a reader experiences differently. Flattening happens
 * once, here, so a component cannot render two states at once or fall through
 * to a default that reads as an answer.
 */
export type HelpViewStatus =
  | "browse"
  | "answer"
  | "ambiguous"
  | "low-confidence"
  | "no-match"
  | "rejected";

export type HelpQueryRejection = "query-too-long" | "query-too-many-bytes" | "control-characters";

/** A quote the corpus reproduced, with the documents backing that exact text. */
export type HelpViewCitation = {
  readonly sourceId: string;
  readonly path: string;
  readonly heading: string;
  readonly verified: true;
};

export type HelpViewCandidate = {
  readonly articleId: string;
  readonly chunkId: string;
  readonly title: string;
  readonly topic: HelpTopic;
  readonly summary: string;
  /** The exact corpus bytes that matched, re-read from the corpus. */
  readonly quote: string;
  readonly matchedTerms: readonly string[];
  /** Fused ranking signal in [0, 1]. A ranking signal, never a certification. */
  readonly score: number;
  readonly citations: readonly HelpViewCitation[];
  /**
   * Citations dropped because the corpus did not reproduce them. Non-zero
   * means the surface is showing fewer sources than retrieval named and must
   * say so rather than implying the list is complete.
   */
  readonly unverifiedCitationCount: number;
};

export type HelpViewState = {
  readonly contract: typeof HELP_VIEW_CONTRACT;
  readonly status: HelpViewStatus;
  readonly corpusDigest: string;
  readonly retrievalMode: "offline-hybrid";
  readonly query: string;
  /** Populated only when status is `answer`; null in every other status. */
  readonly answer: HelpViewCandidate | null;
  /** Ranked candidates. Suggestions, never the response to the question. */
  readonly candidates: readonly HelpViewCandidate[];
  readonly rejection: HelpQueryRejection | null;
  /** The retriever's own abstention reason, carried through unchanged. */
  readonly abstainReason: string | null;
  readonly headline: string;
  readonly detail: string;
};

/**
 * Facts this layer cannot establish and therefore does not assert.
 *
 * Retrieval runs in this process against a corpus compiled into the build, so
 * there is no provider to name, no model to name, no price, and no latency to
 * measure. Recording them as unknown keeps a default from reading as a
 * measurement.
 */
export type HelpViewUnknowns = {
  readonly provider: "unknown";
  readonly model: "unknown";
  readonly cost: "unknown";
  readonly latency: "unknown";
  readonly note: string;
};

export const HELP_VIEW_UNKNOWNS: HelpViewUnknowns = Object.freeze({
  provider: "unknown" as const,
  model: "unknown" as const,
  cost: "unknown" as const,
  latency: "unknown" as const,
  note:
    "Search runs on this machine against the documentation shipped in this build. " +
    "No provider is contacted, so provider identity, model identity, price, and " +
    "latency are not observed here and are not inferable from this result.",
});

/** C0/C1 controls, bidi and zero-width marks, and the BOM. */
function hasControlCharacters(value: string): boolean {
  for (const character of value) {
    const code = character.codePointAt(0) ?? 0;
    if (code < 0x20) return true;
    if (code >= 0x7f && code <= 0x9f) return true;
    if (code >= 0x200b && code <= 0x200f) return true;
    if (code >= 0x2028 && code <= 0x202e) return true;
    if (code >= 0x2066 && code <= 0x2069) return true;
    if (code === 0xfeff) return true;
  }
  return false;
}

function utf8Length(value: string): number {
  return new TextEncoder().encode(value).length;
}

/** Reject a query on a bound before it ever reaches the index. */
export function rejectQuery(query: string): HelpQueryRejection | null {
  if (query.length > HELP_MAX_QUERY_CHARS) return "query-too-long";
  if (utf8Length(query) > HELP_MAX_QUERY_BYTES) return "query-too-many-bytes";
  if (hasControlCharacters(query)) return "control-characters";
  return null;
}

/**
 * Re-resolve one result's quote and sources against the corpus.
 *
 * Retrieval already read these bytes, but a surface that shows a quote is
 * making a claim about a document, and re-reading costs nothing. A source the
 * corpus cannot produce is dropped rather than shown with a broken reference.
 */
function revalidate(
  result: HelpRetrievalResult,
  corpus: HelpCorpus,
): { quote: string; citations: HelpViewCitation[]; unverified: number } | null {
  const chunk = findChunk(corpus, result.chunkId);
  // The chunk itself must still exist and still say what retrieval reported.
  // If it does not, there is nothing honest left to render for this result.
  if (!chunk || chunk.text !== result.text) return null;

  const citations: HelpViewCitation[] = [];
  let unverified = 0;
  for (const sourceId of result.sourceIds) {
    const source: HelpSourceAnchor | undefined = findSource(corpus, sourceId);
    if (!source) {
      unverified += 1;
      continue;
    }
    citations.push({
      sourceId: source.id,
      path: source.path,
      heading: source.heading,
      verified: true,
    });
  }
  return { quote: chunk.text, citations, unverified };
}

function candidateFor(
  result: HelpRetrievalResult,
  corpus: HelpCorpus,
): HelpViewCandidate | null {
  const resolved = revalidate(result, corpus);
  if (!resolved) return null;
  return {
    articleId: result.articleId,
    chunkId: result.chunkId,
    title: result.title,
    topic: result.topic,
    summary: result.summary,
    quote: resolved.quote,
    matchedTerms: result.matchedTerms,
    score: result.score.fused,
    citations: resolved.citations,
    unverifiedCitationCount: resolved.unverified,
  };
}

const REJECTION_COPY: Readonly<Record<HelpQueryRejection, string>> = Object.freeze({
  "query-too-long": "That question is longer than Help accepts. Shorten it and search again.",
  "query-too-many-bytes": "That question is larger than Help accepts. Shorten it and search again.",
  "control-characters": "That question contains control characters, so it was not searched.",
});

const HEADLINE: Readonly<Record<HelpViewStatus, string>> = Object.freeze({
  browse: "Browse the Help corpus",
  answer: "Answer from the shipped documentation",
  ambiguous: "More than one article fits",
  "low-confidence": "No confident answer",
  "no-match": "No documented answer",
  rejected: "Question not searched",
});

const DETAIL: Readonly<Record<Exclude<HelpViewStatus, "rejected">, string>> = Object.freeze({
  browse:
    "Search the documentation shipped in this build. Retrieval runs on this machine; nothing is sent anywhere.",
  answer: "Every quote below was re-read from the corpus before it was shown.",
  ambiguous:
    "Two or more articles scored too closely to call one the answer. They are listed as candidates; none is being presented as the response.",
  "low-confidence":
    "Some articles matched, but none strongly enough to answer. They are listed as candidates, not as an answer.",
  "no-match":
    "The shipped documentation contains nothing matching that question, so Help is not guessing at one.",
});

/**
 * Project a retrieval into the state a surface renders.
 *
 * The retriever's own outcome and abstention reason are carried through
 * unchanged next to the derived status, so a surface can always report what
 * retrieval actually said rather than only this module's wording of it.
 */
export function helpViewState(
  query: string,
  corpus: HelpCorpus,
  options: HelpRetrievalOptions = {},
): HelpViewState {
  const base = {
    contract: HELP_VIEW_CONTRACT,
    corpusDigest: corpus.digest,
    retrievalMode: "offline-hybrid" as const,
    query,
  };

  const rejection = rejectQuery(query);
  if (rejection) {
    return Object.freeze({
      ...base,
      status: "rejected" as const,
      answer: null,
      candidates: Object.freeze([]),
      rejection,
      abstainReason: null,
      headline: HEADLINE.rejected,
      detail: REJECTION_COPY[rejection],
    });
  }

  const outcome: HelpRetrievalOutcome = searchHelpCorpus(query, { ...options, corpus });

  if (outcome.kind === "abstained") {
    // `no-query` is a reader who has not asked yet, not a reader whose question
    // failed, and an empty corpus is indistinguishable to a reader from a
    // corpus that documents nothing relevant.
    const status: HelpViewStatus =
      outcome.reason === "no-query"
        ? "browse"
        : outcome.reason === "below-threshold"
          ? "low-confidence"
          : "no-match";
    return Object.freeze({
      ...base,
      status,
      answer: null,
      candidates: Object.freeze([]),
      rejection: null,
      abstainReason: outcome.reason,
      headline: HEADLINE[status],
      detail: DETAIL[status as Exclude<HelpViewStatus, "rejected">],
    });
  }

  const candidates = outcome.results
    .map((result) => candidateFor(result, corpus))
    .filter((candidate): candidate is HelpViewCandidate => candidate !== null);

  if (candidates.length === 0) {
    // Retrieval found results but the corpus no longer reproduces any of them.
    // Showing nothing is the only honest option left.
    return Object.freeze({
      ...base,
      status: "no-match" as const,
      answer: null,
      candidates: Object.freeze([]),
      rejection: null,
      abstainReason: "unverifiable-results",
      headline: HEADLINE["no-match"],
      detail: DETAIL["no-match"],
    });
  }

  const [leader, runnerUp] = candidates;
  const undecisive = leader.score < HELP_CLEAR_SCORE;
  const tied =
    runnerUp !== undefined && leader.score > 0 && runnerUp.score / leader.score >= HELP_AMBIGUITY_RATIO;

  if (undecisive && tied) {
    return Object.freeze({
      ...base,
      status: "ambiguous" as const,
      answer: null,
      candidates: Object.freeze(candidates),
      rejection: null,
      abstainReason: null,
      headline: HEADLINE.ambiguous,
      detail: DETAIL.ambiguous,
    });
  }

  return Object.freeze({
    ...base,
    status: "answer" as const,
    answer: leader,
    // The leader is the answer, not also a suggestion.
    candidates: Object.freeze(candidates.slice(1)),
    rejection: null,
    abstainReason: null,
    headline: HEADLINE.answer,
    detail: DETAIL.answer,
  });
}

/** The abstention threshold the retriever used, for a surface that shows it. */
export { HELP_ABSTENTION_THRESHOLD };
