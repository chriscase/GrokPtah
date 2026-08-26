/**
 * The Help Center: one surface over one corpus.
 *
 * This replaces two surfaces that answered the same questions from two
 * separately edited corpora — `HelpPanel` over `grokptah.help.v1` and the old
 * `HelpCenter` over `product-corpus-v1`. Two corpora meant two answers to the
 * same question and no way to tell which was current.
 *
 * # Offline first, and offline is the product
 *
 * Search runs in this process against corpus bytes the app already has. It is
 * not a degraded mode for when a provider is unreachable — it is what the Help
 * Center does. Asking the host for a written answer is a separate, explicit
 * action, and the host may abstain; the search results stay on screen either
 * way, so the reader is never left with nothing because a model declined.
 *
 * # It cannot open a chat
 *
 * The previous version answered questions by minting a generic chat session
 * and prompting it. That put Help's content on the ordinary Chat path, with
 * its history, its tools, and its workspace — none of which Help needs and all
 * of which it then had to be trusted not to use. There is no session call in
 * this file. `lib/help/host.ts` is the only way out, and it can send three
 * things: a question, a locale, and handles the host issued.
 */

import { useCallback, useEffect, useId, useMemo, useRef, useState } from "react";

import type { HelpProjection, HelpTopic } from "../lib/help/generated/contract";
import { HELP_CORPUS, getHelpArticle, getHelpSource } from "../lib/help/canonical/corpus";
import {
  HELP_RETRIEVAL_DEFAULT_LIMIT,
  searchHelpCorpus,
  type HelpRetrievalOutcome,
} from "../lib/help/retrieval/hybrid";
import { helpAsk, helpCancel, helpFollow } from "../lib/help/host";
import { verifyHelpProjection } from "../lib/help/verify";

export type HelpCenterProps = {
  open: boolean;
  onClose: () => void;
  /** Opaque session token the host issued. */
  sessionToken: string;
};

const TOPICS: Array<{ value: HelpTopic | "all"; label: string }> = [
  { value: "all", label: "All topics" },
  { value: "getting-started", label: "Getting started" },
  { value: "providers", label: "Providers" },
  { value: "computer-use", label: "Computer Use" },
  { value: "operations", label: "Operations" },
];

const FOCUSABLE_SELECTOR =
  'button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), a[href], [tabindex]:not([tabindex="-1"])';

function focusableIn(root: HTMLElement | null): HTMLElement[] {
  if (!root) return [];
  return Array.from(root.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR)).filter(
    (element) => element.offsetParent !== null || element === document.activeElement,
  );
}

/** What the host is doing about a written answer, as this surface knows it. */
type AnswerState =
  | { status: "idle" }
  | { status: "asking"; handle: string | null }
  | { status: "answered"; projection: HelpProjection }
  | { status: "abstained" }
  | { status: "unavailable"; message: string };

export function HelpCenter({ open, onClose, sessionToken }: HelpCenterProps) {
  const [query, setQuery] = useState("");
  const [topic, setTopic] = useState<HelpTopic | "all">("all");
  const [selectedId, setSelectedId] = useState<string>(HELP_CORPUS.articles[0]?.id ?? "");
  const [answer, setAnswer] = useState<AnswerState>({ status: "idle" });

  const dialogRef = useRef<HTMLDivElement>(null);
  const searchRef = useRef<HTMLInputElement>(null);
  const returnFocusRef = useRef<HTMLElement | null>(null);
  const pollRef = useRef<number | null>(null);

  const titleId = useId();
  const statusId = useId();
  const resultsId = useId();

  // Retrieval is synchronous and local, so results are just derived state.
  const outcome: HelpRetrievalOutcome = useMemo(
    () =>
      searchHelpCorpus(query, {
        topic,
        limit: HELP_RETRIEVAL_DEFAULT_LIMIT,
        corpus: HELP_CORPUS,
      }),
    [query, topic],
  );

  const results = outcome.kind === "results" ? outcome.results : [];

  const browseArticles = useMemo(
    () =>
      HELP_CORPUS.articles.filter(
        (article) => topic === "all" || article.topic === topic,
      ),
    [topic],
  );

  const visibleArticles = query.trim()
    ? results.map((result) => getHelpArticle(result.articleId)).filter((a) => a !== undefined)
    : browseArticles;

  const selected = getHelpArticle(selectedId) ?? visibleArticles[0];

  // Keep the selection inside what is on screen, so the detail pane never
  // shows an article the list no longer offers.
  useEffect(() => {
    if (!visibleArticles.some((article) => article.id === selectedId)) {
      setSelectedId(visibleArticles[0]?.id ?? "");
    }
  }, [visibleArticles, selectedId]);

  const stopPolling = useCallback(() => {
    if (pollRef.current !== null) {
      window.clearTimeout(pollRef.current);
      pollRef.current = null;
    }
  }, []);

  const applyProjection = useCallback((projection: HelpProjection) => {
    // The host already validated. Re-checking here can only remove claims,
    // never add one, and it catches a projection altered in transit.
    const { projection: verified } = verifyHelpProjection(projection, HELP_CORPUS);
    switch (verified.status) {
      case "answered":
        setAnswer({ status: "answered", projection: verified });
        return true;
      case "abstained":
        setAnswer({ status: "abstained" });
        return true;
      case "unavailable":
        setAnswer({
          status: "unavailable",
          message: verified.message ?? "Help cannot answer that right now.",
        });
        return true;
      default:
        setAnswer({ status: "asking", handle: verified.handle });
        return false;
    }
  }, []);

  const poll = useCallback(
    (handle: string) => {
      stopPolling();
      pollRef.current = window.setTimeout(() => {
        void helpFollow({ session: sessionToken, handle })
          .then((projection) => {
            if (!applyProjection(projection)) poll(handle);
          })
          .catch(() => {
            setAnswer({
              status: "unavailable",
              message: "Help cannot answer that right now.",
            });
          });
      }, 250);
    },
    [applyProjection, sessionToken, stopPolling],
  );

  const askForAnswer = useCallback(() => {
    const question = query.trim();
    if (!question) return;
    setAnswer({ status: "asking", handle: null });
    void helpAsk({ session: sessionToken, question, locale: null })
      .then((projection) => {
        if (!applyProjection(projection) && projection.handle) poll(projection.handle);
      })
      .catch(() => {
        setAnswer({ status: "unavailable", message: "Help cannot answer that right now." });
      });
  }, [applyProjection, poll, query, sessionToken]);

  const cancelAnswer = useCallback(() => {
    stopPolling();
    const handle = answer.status === "asking" ? answer.handle : null;
    if (!handle) {
      setAnswer({ status: "idle" });
      return;
    }
    void helpCancel({ session: sessionToken, handle })
      .then(applyProjection)
      .catch(() => setAnswer({ status: "idle" }));
  }, [answer, applyProjection, sessionToken, stopPolling]);

  useEffect(() => stopPolling, [stopPolling]);

  // Reset the answer when the question changes: an answer to a previous
  // question shown beside a new one is a claim about the wrong thing.
  useEffect(() => {
    stopPolling();
    setAnswer({ status: "idle" });
  }, [query, stopPolling]);

  // Focus management: remember the opener, move focus in, restore on close.
  useEffect(() => {
    if (!open) return undefined;
    returnFocusRef.current = document.activeElement as HTMLElement | null;
    const timer = window.setTimeout(() => searchRef.current?.focus(), 0);
    return () => {
      window.clearTimeout(timer);
      returnFocusRef.current?.focus?.();
    };
  }, [open]);

  useEffect(() => {
    if (!open) return undefined;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        onClose();
        return;
      }
      if (event.key !== "Tab") return;
      const focusable = focusableIn(dialogRef.current);
      if (focusable.length === 0) return;
      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      const active = document.activeElement as HTMLElement | null;
      if (event.shiftKey && (active === first || !dialogRef.current?.contains(active))) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && active === last) {
        event.preventDefault();
        first.focus();
      }
    };
    document.addEventListener("keydown", onKeyDown, true);
    return () => document.removeEventListener("keydown", onKeyDown, true);
  }, [open, onClose]);

  if (!open) return null;

  const statusMessage = (() => {
    if (query.trim() && outcome.kind === "abstained") {
      return "No Help article matches that closely enough to show. Try different words, or browse by topic.";
    }
    if (query.trim()) {
      return `${results.length} Help ${results.length === 1 ? "article" : "articles"} match.`;
    }
    return `Browsing ${browseArticles.length} Help ${browseArticles.length === 1 ? "article" : "articles"}.`;
  })();

  return (
    <div className="modal-backdrop help-center-backdrop" onClick={onClose}>
      <div
        className="modal help-center"
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        ref={dialogRef}
        onClick={(event) => event.stopPropagation()}
      >
        <header className="help-center-header">
          <h2 id={titleId}>Help Center</h2>
          <button type="button" onClick={onClose} aria-label="Close Help Center">
            Close
          </button>
        </header>

        <div className="help-center-search">
          <label htmlFor="help-search-input">Search Help</label>
          <input
            id="help-search-input"
            ref={searchRef}
            type="search"
            value={query}
            placeholder="How do I recover an interrupted run?"
            aria-describedby={statusId}
            aria-controls={resultsId}
            onChange={(event) => setQuery(event.target.value)}
          />
          <label htmlFor="help-topic-select">Topic</label>
          <select
            id="help-topic-select"
            value={topic}
            onChange={(event) => setTopic(event.target.value as HelpTopic | "all")}
          >
            {TOPICS.map((entry) => (
              <option key={entry.value} value={entry.value}>
                {entry.label}
              </option>
            ))}
          </select>
        </div>

        <p className="help-center-offline-note">
          Search runs on this device against the Help content shipped with the app. Nothing
          leaves your computer unless you ask for a written answer.
        </p>

        <p id={statusId} role="status" aria-live="polite" className="help-center-status">
          {statusMessage}
        </p>

        <div className="help-center-body">
          <nav aria-label="Help articles" className="help-center-list">
            <ul id={resultsId}>
              {visibleArticles.map((article) => (
                <li key={article.id}>
                  <button
                    type="button"
                    aria-current={article.id === selected?.id ? "true" : undefined}
                    onClick={() => setSelectedId(article.id)}
                  >
                    <span className="help-article-title">{article.title}</span>
                    <span className="help-article-summary">{article.summary}</span>
                  </button>
                </li>
              ))}
            </ul>
            {visibleArticles.length === 0 && (
              <p className="help-center-empty">Nothing to show for this search.</p>
            )}
          </nav>

          <article className="help-center-detail" aria-label="Help article">
            {selected ? (
              <>
                <h3>{selected.title}</h3>
                <p className="help-article-summary">{selected.summary}</p>
                <p>{selected.body}</p>
                <h4>Sources</h4>
                <ul className="help-article-sources">
                  {selected.source_ids.map((sourceId) => {
                    const source = getHelpSource(sourceId);
                    if (!source) return null;
                    return (
                      <li key={sourceId}>
                        <code>{source.path}</code> — {source.heading}
                      </li>
                    );
                  })}
                </ul>
              </>
            ) : (
              <p>Select an article to read it.</p>
            )}
          </article>
        </div>

        <section className="help-center-answer" aria-label="Written answer">
          <div className="help-center-answer-controls">
            <button
              type="button"
              onClick={askForAnswer}
              disabled={!query.trim() || answer.status === "asking"}
            >
              Ask for a written answer
            </button>
            {answer.status === "asking" && (
              <button type="button" onClick={cancelAnswer}>
                Stop
              </button>
            )}
          </div>

          <p role="status" aria-live="polite" className="help-center-answer-status">
            {answer.status === "idle" &&
              "A written answer is a separate step. It is checked against the Help content before it is shown."}
            {answer.status === "asking" && "Working on a written answer. You can keep reading."}
            {answer.status === "abstained" &&
              "Help could not support an answer from the shipped content, so it did not write one. The search results above are unaffected."}
            {answer.status === "unavailable" && answer.message}
            {answer.status === "answered" &&
              `${answer.projection.claims.length} checked ${answer.projection.claims.length === 1 ? "statement" : "statements"}.`}
          </p>

          {answer.status === "answered" && (
            <ol className="help-center-claims">
              {answer.projection.claims.map((claim) => (
                <li key={claim.ordinal}>
                  <p>{claim.text}</p>
                  <ul className="help-center-citations" aria-label="Sources for this statement">
                    {claim.citations.map((citation, index) => (
                      <li key={`${citation.source_id}-${index}`}>
                        <code>{citation.path}</code> — {citation.heading}
                        <blockquote>{citation.quote}</blockquote>
                      </li>
                    ))}
                  </ul>
                </li>
              ))}
            </ol>
          )}
        </section>
      </div>
    </div>
  );
}
