/**
 * Consumer view contract for the canonical Help authority.
 *
 * `grokptah.help-authority.v1` decides what the documentation says and when it
 * does not know. This module decides how a *consumer* is allowed to present
 * that decision, and nothing else. It re-derives no ranking, re-validates no
 * reply, and restates no threshold: every judgement here is read off a
 * `HelpRetrievalResult` the authority already produced.
 *
 * It exists because the dangerous part of a Help UI is not retrieval — it is
 * the gap between "the retriever abstained" and what the screen says. Three
 * rules close that gap, and each is enforced here rather than left to a
 * component:
 *
 *   - **An abstention is never an answer.** `state.answer` is populated for
 *     exactly one status, `answer`. Every other status carries candidates,
 *     which a consumer may offer as suggestions and must not present as the
 *     response to the question.
 *   - **A citation is verified before it is shown.** Every span is re-resolved
 *     through `authority.resolveSpan` and dropped if the corpus does not agree
 *     with it, with the drop counted so the UI can say so rather than quietly
 *     render fewer quotes.
 *   - **A documented capability is not a granted one.** Capability and access
 *     labels describe what an article documents. Live availability is
 *     `"unknown"` at this layer, and saying otherwise would be a claim this
 *     code cannot support.
 *
 * Like the authority, this module is pure, synchronous, transport-free, and
 * Tauri-free: no fetch, no clock, no randomness, no native binding. React is
 * not imported here on purpose, so an embedder with its own visual language
 * can consume the same states GrokPtah's desktop Help Center consumes.
 */

import type {
  HelpAnswerRequest,
  HelpAnswerResponse,
  HelpAnswerValidation,
} from "./helpAnswer";
import type {
  HelpAbstainReason,
  HelpAuthority,
  HelpAuthorityAccess,
  HelpAuthorityArticle,
  HelpAuthorityAudience,
  HelpCitationSpan,
  HelpHit,
  HelpQueryRejection,
  HelpRetrievalOutcome,
  HelpRetrievalResult,
  HelpSearchRequest,
} from "./helpAuthority";
import type { HelpSource, HelpTopic } from "./helpCenter";

export const HELP_CENTER_VIEW_CONTRACT = "grokptah.help-center-view.v1" as const;

/* ------------------------------------------------------------------ *
 * Presentation states
 * ------------------------------------------------------------------ */

/**
 * The one status a consumer renders from.
 *
 * This flattens the authority's `(outcome, abstainReason, rejection)` triple
 * into mutually exclusive cases so a UI cannot accidentally render two of them
 * at once, or fall through to a default that reads as an answer. `browse` is
 * separated from the other abstentions deliberately: an empty query is a
 * reader who has not asked yet, not a reader whose question failed.
 */
export type HelpViewStatus =
  | "browse"
  | "answer"
  | "ambiguous"
  | "no-match"
  | "low-confidence"
  | "rejected";

/** Statuses in which candidates exist but must not be shown as the answer. */
export const HELP_VIEW_SUGGESTION_STATUSES: readonly HelpViewStatus[] = Object.freeze([
  "ambiguous",
  "low-confidence",
]);

export type HelpAccessLabel = {
  readonly value: HelpAuthorityAccess;
  readonly label: string;
  /** Why the label is worded this way, including what it does not grant. */
  readonly detail: string;
};

export type HelpAudienceLabel = {
  readonly value: HelpAuthorityAudience;
  readonly label: string;
};

/**
 * A capability an article documents.
 *
 * `documented` is always true and `liveAvailability` is always `"unknown"`:
 * this layer reads a corpus, not a live authority. A consumer that wants to
 * offer the operation must make its own check first.
 */
export type HelpCapabilityLabel = {
  readonly id: string;
  readonly label: string;
  readonly documented: true;
  readonly liveAvailability: "unknown";
};

export type HelpViewLabels = {
  readonly access: HelpAccessLabel;
  readonly audience: readonly HelpAudienceLabel[];
  readonly capabilities: readonly HelpCapabilityLabel[];
  /** One sentence a consumer must render wherever capabilities are shown. */
  readonly liveAvailabilityNote: string;
};

/** A citation span the corpus re-confirmed, quote for quote. */
export type HelpViewSpan = {
  readonly articleId: string;
  readonly field: HelpCitationSpan["field"];
  readonly passageId: string | null;
  readonly start: number;
  readonly end: number;
  readonly quote: string;
  readonly term: string;
  /** The documents backing this exact span, not the article's union. */
  readonly sources: readonly HelpSource[];
  readonly verified: true;
};

export type HelpViewPassage = {
  readonly id: string;
  readonly text: string;
  readonly sources: readonly HelpSource[];
};

export type HelpViewArticle = {
  readonly articleId: string;
  readonly title: string;
  readonly summary: string;
  readonly topic: HelpTopic;
  readonly confidence: number;
  readonly coverage: number;
  readonly matchedTerms: readonly string[];
  readonly passages: readonly HelpViewPassage[];
  /** Verified spans only. */
  readonly spans: readonly HelpViewSpan[];
  /**
   * Spans the corpus did not confirm, dropped rather than rendered. Non-zero
   * means the UI is showing fewer quotes than retrieval produced and must say
   * so instead of implying the citation set is complete.
   */
  readonly unverifiedSpanCount: number;
  readonly sources: readonly HelpSource[];
  readonly labels: HelpViewLabels;
};

export type HelpViewCandidate = {
  readonly articleId: string;
  readonly title: string;
  readonly summary: string;
  readonly topic: HelpTopic;
  readonly confidence: number;
  readonly matchedTerms: readonly string[];
  readonly labels: HelpViewLabels;
};

export type HelpViewState = {
  readonly contract: typeof HELP_CENTER_VIEW_CONTRACT;
  readonly corpusVersion: string;
  readonly digest: string;
  readonly retrievalMode: "offline-hybrid";
  readonly status: HelpViewStatus;
  /** The authority's own verdict, carried through unchanged. */
  readonly outcome: HelpRetrievalOutcome;
  readonly abstainReason: HelpAbstainReason | null;
  readonly rejection: HelpQueryRejection | null;
  readonly query: string;
  readonly queryTerms: readonly string[];
  /** Populated only when status is `answer`; null in every other status. */
  readonly answer: HelpViewArticle | null;
  /** Ranked candidates. Suggestions, never the response to the question. */
  readonly candidates: readonly HelpViewCandidate[];
  readonly totalMatched: number;
  readonly limit: number;
  /** Short state line, safe to announce in a live region. */
  readonly headline: string;
  /** What the reader should do next, or why nothing is being claimed. */
  readonly detail: string;
  /**
   * Whether the optional model seam may be offered at all. False for every
   * status but `answer`, mirroring `buildHelpAnswerRequest`'s own refusal.
   */
  readonly canAskModel: boolean;
};

/* ------------------------------------------------------------------ *
 * Labels
 * ------------------------------------------------------------------ */

const ACCESS_LABELS: Readonly<Record<HelpAuthorityAccess, Omit<HelpAccessLabel, "value">>> =
  Object.freeze({
    public: Object.freeze({
      label: "Open to everyone",
      detail: "Documented for any reader. No approval is described.",
    }),
    gated: Object.freeze({
      label: "Needs approval",
      detail:
        "Documented behind an approval. Reading this article does not grant it.",
    }),
    operator: Object.freeze({
      label: "Operator only",
      detail:
        "Documented for operators. Reading this article does not confer the role.",
    }),
  });

const AUDIENCE_LABELS: Readonly<Record<HelpAuthorityAudience, string>> = Object.freeze({
  everyone: "Everyone",
  power_user: "Power user",
  operator: "Operator",
});

/**
 * Human wording for the capability vocabulary the shipped corpus documents.
 *
 * An ID outside this table is formatted from its own segments rather than
 * dropped or renamed, so a consumer-supplied corpus stays legible without this
 * table pretending to know what its capabilities mean.
 */
const CAPABILITY_LABELS: Readonly<Record<string, string>> = Object.freeze({
  "agent.continuity": "Agent continuity",
  "agent.resume": "Resume an agent",
  "computer.control": "Control the computer",
  "computer.observe": "Observe the screen",
  "run.execute": "Execute a run",
  "run.promote": "Promote a run",
  "run.queue": "Queue a run",
  "run.review": "Review a run",
  "session.observe": "Observe sessions",
});

export const HELP_LIVE_AVAILABILITY_NOTE =
  "Documented capability, not a live grant: availability, approval, lease, and " +
  "quota are checked elsewhere and are unknown here.";

/** Title-case one dotted segment without guessing at unknown vocabulary. */
function humanizeSegment(segment: string): string {
  const spaced = segment.replace(/[_-]+/g, " ").trim();
  if (spaced.length === 0) return segment;
  return spaced.charAt(0).toUpperCase() + spaced.slice(1);
}

export function helpCapabilityLabel(id: string): string {
  const known = CAPABILITY_LABELS[id];
  if (known !== undefined) return known;
  const segments = id.split(".").filter((segment) => segment.length > 0);
  if (segments.length === 0) return id;
  return segments.map(humanizeSegment).join(" · ");
}

export function helpAccessLabel(access: HelpAuthorityAccess): HelpAccessLabel {
  const entry = ACCESS_LABELS[access];
  // An access value outside the vocabulary is reported as restricted rather
  // than as "open": an unknown restriction must never read as no restriction.
  if (entry === undefined) {
    return Object.freeze({
      value: access,
      label: "Restricted",
      detail: "This article declares an access level this consumer does not recognise.",
    });
  }
  return Object.freeze({ value: access, ...entry });
}

export function helpAudienceLabel(audience: HelpAuthorityAudience): HelpAudienceLabel {
  return Object.freeze({
    value: audience,
    label: AUDIENCE_LABELS[audience] ?? humanizeSegment(audience),
  });
}

export function helpViewLabels(article: HelpAuthorityArticle): HelpViewLabels {
  return Object.freeze({
    access: helpAccessLabel(article.access),
    audience: Object.freeze(article.audience.map(helpAudienceLabel)),
    capabilities: Object.freeze(
      article.capabilityIds.map((id) =>
        Object.freeze({
          id,
          label: helpCapabilityLabel(id),
          documented: true as const,
          liveAvailability: "unknown" as const,
        })),
    ),
    liveAvailabilityNote: HELP_LIVE_AVAILABILITY_NOTE,
  });
}

/* ------------------------------------------------------------------ *
 * State copy
 * ------------------------------------------------------------------ */

const REJECTION_COPY: Readonly<Record<HelpQueryRejection, string>> = Object.freeze({
  "not-a-string": "The search input was not text.",
  "query-too-long": "That question is longer than Help accepts. Shorten it and search again.",
  "query-too-many-bytes": "That question is larger than Help accepts. Shorten it and search again.",
  "control-characters": "That question contains control characters, so it was not searched.",
  "invalid-limit": "The result limit was out of range, so nothing was searched.",
  "invalid-audience": "The audience filter was not recognised, so nothing was searched.",
  "invalid-topic": "The topic filter was not recognised, so nothing was searched.",
});

const ABSTAIN_HEADLINE: Readonly<Record<HelpAbstainReason, string>> = Object.freeze({
  "empty-query": "Browse the Help corpus",
  "no-match": "No documented answer",
  "low-confidence": "No confident answer",
  ambiguous: "More than one article fits",
});

function detailFor(status: HelpViewStatus, result: HelpRetrievalResult): string {
  switch (status) {
    case "browse":
      return "Search the shipped documentation. Retrieval runs offline; nothing is sent anywhere.";
    case "answer":
      return "Every quote below is re-checked against the corpus before it is shown.";
    case "ambiguous":
      return (
        "Two or more articles scored too closely to call one the answer. " +
        "They are listed as candidates; none is being presented as the response."
      );
    case "no-match":
      return (
        "The shipped documentation contains nothing matching that question, " +
        "so Help is not guessing at one."
      );
    case "low-confidence":
      return result.totalMatched > 0
        ? "Some articles matched weakly. They are listed as candidates, not as an answer."
        : "Nothing matched strongly enough to answer, so nothing is being claimed.";
    case "rejected":
      return result.rejection === null
        ? "The question was not searched."
        : REJECTION_COPY[result.rejection] ?? "The question was not searched.";
    default:
      return "";
  }
}

function headlineFor(status: HelpViewStatus, result: HelpRetrievalResult): string {
  if (status === "answer") return "Answer from the shipped documentation";
  if (status === "rejected") return "Question not searched";
  if (result.abstainReason !== null) {
    return ABSTAIN_HEADLINE[result.abstainReason] ?? "No answer";
  }
  return "No answer";
}

/** Flatten the authority's verdict into exactly one presentation status. */
export function helpViewStatus(result: HelpRetrievalResult): HelpViewStatus {
  if (result.outcome === "rejected") return "rejected";
  if (result.outcome === "answer") return "answer";
  switch (result.abstainReason) {
    case "empty-query":
      return "browse";
    case "no-match":
      return "no-match";
    case "ambiguous":
      return "ambiguous";
    case "low-confidence":
      return "low-confidence";
    default:
      // An abstention this consumer cannot name is still an abstention. It
      // degrades to the weakest presentable state, never to an answer.
      return "low-confidence";
  }
}

/* ------------------------------------------------------------------ *
 * Citations
 * ------------------------------------------------------------------ */

/**
 * Keep only the spans the corpus re-confirms.
 *
 * A span whose quote the authority cannot reproduce is not a weaker citation,
 * it is an unverifiable one, so it is dropped instead of rendered. The count
 * of drops is returned alongside so a consumer can disclose the gap.
 */
export function verifyHelpSpans(
  spans: readonly HelpCitationSpan[],
  authority: Pick<HelpAuthority, "resolveSpan">,
): { readonly spans: readonly HelpViewSpan[]; readonly unverified: number } {
  const verified: HelpViewSpan[] = [];
  let unverified = 0;
  for (const span of spans) {
    if (authority.resolveSpan(span) !== span.quote) {
      unverified += 1;
      continue;
    }
    verified.push(Object.freeze({
      articleId: span.articleId,
      field: span.field,
      passageId: span.passageId,
      start: span.start,
      end: span.end,
      quote: span.quote,
      term: span.term,
      sources: span.sources,
      verified: true as const,
    }));
  }
  return { spans: Object.freeze(verified), unverified };
}

function candidateFor(hit: HelpHit): HelpViewCandidate {
  return Object.freeze({
    articleId: hit.article.id,
    title: hit.article.title,
    summary: hit.article.summary,
    topic: hit.article.topic,
    confidence: hit.confidence,
    matchedTerms: hit.matchedTerms,
    labels: helpViewLabels(hit.article),
  });
}

export function helpArticleView(
  hit: HelpHit,
  authority: Pick<HelpAuthority, "resolveSpan">,
): HelpViewArticle {
  const { spans, unverified } = verifyHelpSpans(hit.citation.spans, authority);
  return Object.freeze({
    articleId: hit.article.id,
    title: hit.article.title,
    summary: hit.article.summary,
    topic: hit.article.topic,
    confidence: hit.confidence,
    coverage: hit.explanation.coverage,
    matchedTerms: hit.matchedTerms,
    passages: Object.freeze(hit.article.passages.map((passage) =>
      Object.freeze({ id: passage.id, text: passage.text, sources: passage.sources }))),
    spans,
    unverifiedSpanCount: unverified,
    sources: hit.article.sources,
    labels: helpViewLabels(hit.article),
  });
}

/**
 * Project one retrieval result into the state a consumer renders.
 *
 * The projection is total and lossless in the direction that matters: the
 * authority's own outcome, abstain reason, and rejection are carried through
 * unchanged next to the derived status, so a consumer can always report what
 * the retriever actually said rather than only this module's wording of it.
 */
export function helpViewState(
  result: HelpRetrievalResult,
  authority: Pick<HelpAuthority, "resolveSpan">,
): HelpViewState {
  const status = helpViewStatus(result);
  const top = result.hits[0];
  return Object.freeze({
    contract: HELP_CENTER_VIEW_CONTRACT,
    corpusVersion: result.corpusVersion,
    digest: result.digest,
    retrievalMode: result.retrievalMode,
    status,
    outcome: result.outcome,
    abstainReason: result.abstainReason,
    rejection: result.rejection,
    query: result.query,
    queryTerms: result.queryTerms,
    answer: status === "answer" && top ? helpArticleView(top, authority) : null,
    // An answer's leader is the answer, not also a suggestion; every other
    // status offers the full ranked list as candidates.
    candidates: Object.freeze(
      (status === "answer" ? result.hits.slice(1) : result.hits).map(candidateFor),
    ),
    totalMatched: result.totalMatched,
    limit: result.limit,
    headline: headlineFor(status, result),
    detail: detailFor(status, result),
    canAskModel: status === "answer",
  });
}

/**
 * The corpus a consumer lists when there is no query yet.
 *
 * Retrieval has no "return everything" entry point — searching for nothing is
 * an abstention, not a listing — so browse selection lives here. It applies
 * the same three declarative filters a `HelpSearchRequest` applies (topic,
 * audience, and access), in the same direction, so the list a reader browses
 * and the list they can retrieve from are the same set. `helpCenterView.test.ts`
 * asserts that agreement against the authority rather than assuming it.
 *
 * `includeRestricted` is the caller's declaration about its own viewer. It
 * widens what is listed and grants nothing: a gated article shown here is
 * still gated.
 *
 * Order is topic-first, then article ID by code point. Retrieval's own order
 * is by score, which a queryless listing does not have; falling back to the
 * corpus's storage order would put an alphabetical accident at the top of the
 * page. Ties break on code point, not `localeCompare`, for the same reason the
 * authority does: a reading order must not shift with the host locale.
 */
const BROWSE_TOPIC_ORDER: readonly HelpTopic[] = Object.freeze([
  "getting-started", "providers", "computer-use", "operations",
]);

export function helpBrowseArticles(
  articles: readonly HelpAuthorityArticle[],
  request: Pick<HelpSearchRequest, "topic" | "audience" | "includeRestricted"> = {},
): readonly HelpViewCandidate[] {
  const topicRank = (topic: HelpTopic): number => {
    const index = BROWSE_TOPIC_ORDER.indexOf(topic);
    return index === -1 ? BROWSE_TOPIC_ORDER.length : index;
  };
  return Object.freeze(articles
    .filter((article) => {
      if (request.topic && request.topic !== "all" && article.topic !== request.topic) return false;
      if (!request.includeRestricted && article.access !== "public") return false;
      if (request.audience && !article.audience.includes(request.audience)) return false;
      return true;
    })
    .slice()
    .sort((a, b) =>
      topicRank(a.topic) - topicRank(b.topic) ||
      (a.id < b.id ? -1 : a.id > b.id ? 1 : 0))
    .map((article) => Object.freeze({
      articleId: article.id,
      title: article.title,
      summary: article.summary,
      topic: article.topic,
      // Browsing is not ranking. A zero here is the absence of a query, not a
      // weak match, and a consumer must not render it as a score.
      confidence: 0,
      matchedTerms: Object.freeze([]),
      labels: helpViewLabels(article),
    })));
}

/* ------------------------------------------------------------------ *
 * Optional model seam: presentation only
 * ------------------------------------------------------------------ */

/**
 * What a consumer shows about the optional model seam at each step.
 *
 * `unavailable` is distinct from `idle`: a build with no adapter wired must
 * say the seam is absent rather than offer a button that cannot work.
 * `timeout` is distinct from `failed`: a request that outran its declared
 * budget is not evidence that the provider failed, and the UI must not report
 * it as one.
 */
export type HelpAskStatus =
  | "unavailable"
  | "idle"
  | "confirm"
  | "pending"
  | "answered"
  | "declined"
  | "rejected"
  | "timeout"
  | "failed";

export type HelpAskSummary = {
  readonly status: HelpAskStatus;
  readonly headline: string;
  readonly detail: string;
  /** True when the corpus, not the reply, is what the reader should trust. */
  readonly corpusRemainsAuthority: boolean;
};

const VALIDATION_COPY: Readonly<Record<HelpAnswerValidation["reason"], string>> = Object.freeze({
  accepted: "The reply stayed inside the cited bundle.",
  "empty-answer": "The reply claimed an answer but contained no text.",
  "missing-citation": "The reply answered without citing anything, so it was not shown.",
  "unknown-citation": "The reply cited a source that was not in the request, so it was not shown.",
  "missing-uncertainty": "The reply omitted its uncertainty note, so it was not shown.",
  "answer-too-large": "The reply exceeded the size this seam accepts, so it was not shown.",
  "too-many-citations": "The reply carried more citations than this seam accepts.",
  "citation-without-answer": "The reply refused to answer but still cited sources, so it was not shown.",
});

/**
 * Describe a parsed and validated reply without ever upgrading it.
 *
 * A rejected reply and a refusal both leave the cited article as the
 * authority; only an accepted `answered` reply is presented as an answer, and
 * even then the corpus remains the thing the reader can check.
 */
export function summarizeHelpAnswer(
  response: HelpAnswerResponse,
  validation: HelpAnswerValidation,
): HelpAskSummary {
  if (!validation.accepted) {
    return Object.freeze({
      status: "rejected" as const,
      headline: "Reply not shown",
      detail: VALIDATION_COPY[validation.reason] ?? "The reply was not accepted.",
      corpusRemainsAuthority: true,
    });
  }
  if (response.outcome === "answered") {
    return Object.freeze({
      status: "answered" as const,
      headline: "Cited draft answer",
      detail:
        "Drafted from the cited articles only. The cited documentation remains the authority.",
      corpusRemainsAuthority: true,
    });
  }
  return Object.freeze({
    status: "declined" as const,
    headline: response.outcome === "not_found"
      ? "The model found no answer in the cited articles"
      : "The model abstained",
    detail: response.uncertainty,
    corpusRemainsAuthority: true,
  });
}

export type HelpAskUnknowns = {
  readonly provider: string;
  readonly model: string;
  readonly cost: string;
  readonly latency: string;
  readonly note: string;
};

/**
 * Restate the request's own unknowns for display.
 *
 * The values are read from the request rather than composed here, so a UI
 * cannot drift into naming a provider or model this layer never observed.
 * A caller-supplied provider label is a *routing* label the embedder chose; it
 * is never presented as the identity of whatever actually answers.
 */
export function describeHelpAskUnknowns(
  request: HelpAnswerRequest,
  providerLabel?: string,
): HelpAskUnknowns {
  return Object.freeze({
    provider: providerLabel
      ? `${providerLabel} (route chosen by this app; identity unverified)`
      : request.unknowns.provider,
    model: request.unknowns.model,
    cost: request.unknowns.cost,
    latency: request.unknowns.latency,
    note: request.unknowns.note,
  });
}

/**
 * Wording for a request that outran the budget the request itself declared.
 *
 * The timeout is reported against `request.timeoutMs` because that number is
 * knowable and was chosen up front. No elapsed time is asserted: this layer
 * does not measure latency, and a rounded guess would read as a measurement.
 */
export function describeHelpAskTimeout(request: HelpAnswerRequest): HelpAskSummary {
  const seconds = request.timeoutMs / 1_000;
  const budget = Number.isInteger(seconds) ? `${seconds}s` : `${request.timeoutMs}ms`;
  return Object.freeze({
    status: "timeout" as const,
    headline: "No reply within the declared budget",
    detail:
      `The request declared a ${budget} budget and nothing arrived inside it, so it was ` +
      "abandoned. Whether it was ever served is unknown. The cited documentation is unchanged.",
    corpusRemainsAuthority: true,
  });
}
