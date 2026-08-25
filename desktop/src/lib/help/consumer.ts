/**
 * Framework-agnostic Help search controller.
 *
 * A small subscribe/dispatch state machine so a consumer can drive retrieval
 * from React, another framework, or none at all. It owns cancellation of the
 * in-flight query and carries the corpus digest on every state, so a UI can
 * always show exactly which corpus produced what it is displaying.
 */
import { HELP_CORPUS_DIGEST } from "./canonical/corpus";
import {
  searchHelpCorpus,
  type HelpAbstentionReason,
  type HelpRetrievalOptions,
  type HelpRetrievalOutcome,
  type HelpRetrievalResult,
} from "./retrieval/hybrid";

export type HelpSearchState = {
  readonly query: string;
  readonly results: readonly HelpRetrievalResult[];
  readonly abstained: boolean;
  readonly abstentionReason: HelpAbstentionReason;
  readonly confidence: number;
  readonly corpusDigest: string;
  readonly corrections: readonly { readonly from: string; readonly to: string }[];
  readonly redactedQuery: boolean;
  /** Index into `results`, or -1. Owned here so keyboard nav is portable. */
  readonly activeIndex: number;
  readonly searching: boolean;
};

export type HelpSearchController = {
  getState: () => HelpSearchState;
  subscribe: (listener: (state: HelpSearchState) => void) => () => void;
  search: (query: string) => void;
  clear: () => void;
  /** Move the active result by `delta`, clamped to the result list. */
  moveActive: (delta: number) => void;
  setActiveIndex: (index: number) => void;
  /** Cancel any in-flight query and drop all listeners. */
  dispose: () => void;
};

const EMPTY_STATE: HelpSearchState = Object.freeze({
  query: "",
  results: Object.freeze([]),
  abstained: false,
  abstentionReason: "none" as HelpAbstentionReason,
  confidence: 0,
  corpusDigest: HELP_CORPUS_DIGEST,
  corrections: Object.freeze([]),
  redactedQuery: false,
  activeIndex: -1,
  searching: false,
});

function stateFromOutcome(outcome: HelpRetrievalOutcome): HelpSearchState {
  return Object.freeze({
    query: outcome.query,
    results: outcome.results,
    abstained: outcome.abstained,
    abstentionReason: outcome.abstentionReason,
    confidence: outcome.confidence,
    corpusDigest: outcome.corpusDigest,
    corrections: outcome.corrections,
    redactedQuery: outcome.redactions.length > 0,
    activeIndex: outcome.results.length > 0 ? 0 : -1,
    searching: false,
  });
}

/**
 * Create a Help search controller.
 *
 * Retrieval is synchronous and offline, so `search` resolves immediately; the
 * `searching` flag and the abort plumbing exist so a consumer can drive the
 * same controller from an async host without changing shape.
 */
export function createHelpSearchController(
  options: HelpRetrievalOptions = {},
): HelpSearchController {
  let state = EMPTY_STATE;
  let listeners: Array<(next: HelpSearchState) => void> = [];
  let inFlight: AbortController | null = null;
  let disposed = false;

  const emit = (next: HelpSearchState) => {
    state = next;
    for (const listener of [...listeners]) listener(state);
  };

  return {
    getState: () => state,
    subscribe(listener) {
      listeners.push(listener);
      return () => {
        listeners = listeners.filter((candidate) => candidate !== listener);
      };
    },
    search(query) {
      if (disposed) return;
      inFlight?.abort();
      const controller = new AbortController();
      inFlight = controller;
      const outcome = searchHelpCorpus(query, { ...options, signal: controller.signal });
      if (controller.signal.aborted) return;
      inFlight = null;
      emit(stateFromOutcome(outcome));
    },
    clear() {
      if (disposed) return;
      inFlight?.abort();
      inFlight = null;
      emit(EMPTY_STATE);
    },
    moveActive(delta) {
      if (disposed || state.results.length === 0) return;
      const next = Math.max(0, Math.min(state.activeIndex + delta, state.results.length - 1));
      if (next === state.activeIndex) return;
      emit(Object.freeze({ ...state, activeIndex: next }));
    },
    setActiveIndex(index) {
      if (disposed || state.results.length === 0) return;
      const next = Math.max(-1, Math.min(index, state.results.length - 1));
      if (next === state.activeIndex) return;
      emit(Object.freeze({ ...state, activeIndex: next }));
    },
    dispose() {
      disposed = true;
      inFlight?.abort();
      inFlight = null;
      listeners = [];
    },
  };
}

/**
 * One-line description of a result for a screen reader.
 *
 * Built from the result's semantic metadata rather than from rendered layout,
 * so a consumer announces position, topic, and provenance identically however
 * it chooses to display them. Score components are deliberately omitted:
 * "0.43 fused" is noise in a live region.
 */
export function describeHelpResultForAssistiveTech(
  result: HelpRetrievalResult,
  total: number,
): string {
  const sources = result.citations.map((citation) => citation.heading);
  const unique = [...new Set(sources)];
  const cited =
    unique.length === 0
      ? "no cited source"
      : `cited from ${unique.slice(0, 3).join(", ")}${unique.length > 3 ? ", and more" : ""}`;
  return `Result ${result.rank} of ${total}. ${result.title}. Topic ${result.topic.replace(/-/g, " ")}. ${cited}.`;
}
