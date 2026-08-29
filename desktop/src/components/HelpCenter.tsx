/**
 * The Help surface.
 *
 * Retrieval, abstention, and citation verification belong to
 * `lib/help/retrieval` and `lib/help/canonical`; which of those a reader may
 * be shown belongs to `lib/help/view`. This file owns what is left, and only
 * what is left: focus, keyboard, live regions, and layout.
 *
 * Three properties are load-bearing and covered by tests:
 *
 *   - **An abstention never renders as an answer.** The view state carries an
 *     answer for exactly one status; every other status renders candidates
 *     under a banner that says what they are.
 *   - **Nothing leaves the machine.** Search runs in this process against the
 *     corpus compiled into the build. There is no spinner because there is no
 *     wait, and the surface says so rather than implying a service answered.
 *   - **Unknowns stay unknown.** Provider, model, cost, and latency are
 *     rendered as unknown, from the view contract's own constant, because
 *     nothing here observes them.
 *
 * Asking a model for a written answer is a separate action and is disabled in
 * this build: the host's provider seam refuses every request. The surface
 * reports that as a fact about the build, not as a failure of the reader's
 * question, and offline retrieval is the product either way.
 */

import { useCallback, useEffect, useId, useMemo, useRef, useState } from "react";

import type { HelpCorpus, HelpTopic } from "../lib/help/generated/contract";
import { HELP_CORPUS } from "../lib/help/canonical/corpus";
import {
  HELP_VIEW_UNKNOWNS,
  helpViewState,
  type HelpViewCandidate,
} from "../lib/help/view";

export type HelpCenterProps = {
  open: boolean;
  onClose: () => void;
  /**
   * Corpus to search. Defaults to the public corpus compiled into this build;
   * the host supplies a wider one for a principal entitled to more, and the
   * renderer never widens it itself.
   */
  corpus?: HelpCorpus;
  /**
   * Whether the host's provider seam can serve a written answer. False in this
   * build, and rendered as a property of the build rather than as an error.
   */
  answersEnabled?: boolean;
  /**
   * Query to open with, for a caller that already knows what the reader is
   * looking for — an error message linking into Help, or a capture harness.
   * It seeds the field and is editable from the first keystroke; it is not a
   * controlled value, so Help never fights the reader for the input.
   */
  initialQuery?: string;
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
    (element) => !element.closest("[inert]"),
  );
}

function topicLabel(topic: HelpTopic): string {
  return topic.replace(/-/g, " ");
}

/** Percentages are a ranking signal. They are always labelled as one. */
function rankSignal(score: number): string {
  return `${Math.round(score * 100)}%`;
}

export function HelpCenter({
  open,
  onClose,
  corpus,
  answersEnabled = false,
  initialQuery = "",
}: HelpCenterProps) {
  const [query, setQuery] = useState(initialQuery);
  const [topic, setTopic] = useState<HelpTopic | "all">("all");
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [activeIndex, setActiveIndex] = useState(0);

  const dialogRef = useRef<HTMLDivElement>(null);
  const listRef = useRef<HTMLUListElement>(null);
  const returnFocusRef = useRef<HTMLElement | null>(null);
  const onCloseRef = useRef(onClose);
  onCloseRef.current = onClose;

  const listboxId = useId();
  const optionId = useCallback(
    (chunkId: string) => `${listboxId}-${chunkId.replace(/[^a-zA-Z0-9-]/g, "_")}`,
    [listboxId],
  );

  const searched = corpus ?? HELP_CORPUS;
  const view = useMemo(
    () => helpViewState(query, searched, { topic }),
    [query, searched, topic],
  );

  /**
   * What the reader can pick from.
   *
   * In `answer` the leader heads the list and its candidates follow; in an
   * abstention there is no leader, only candidates; a rejected query lists
   * nothing, because it was never searched.
   */
  const listed: readonly HelpViewCandidate[] = useMemo(
    () => (view.answer ? [view.answer, ...view.candidates] : view.candidates),
    [view],
  );

  const selected = useMemo(
    () => listed.find((candidate) => candidate.chunkId === selectedId) ?? listed[0] ?? null,
    [listed, selectedId],
  );

  const isPresentedAnswer =
    view.status === "answer" && selected !== null && selected.chunkId === view.answer?.chunkId;

  useEffect(() => {
    setActiveIndex((index) => (index < listed.length ? index : 0));
  }, [listed.length]);

  /* ------------------------- focus and keyboard -------------------------- */

  useEffect(() => {
    if (!open) return;
    returnFocusRef.current =
      document.activeElement instanceof HTMLElement ? document.activeElement : null;
    dialogRef.current?.querySelector<HTMLElement>("#help-search-input")?.focus();

    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        onCloseRef.current();
        return;
      }
      if (event.key !== "Tab") return;
      const focusable = focusableIn(dialogRef.current);
      if (focusable.length === 0) return;
      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      if (event.shiftKey && document.activeElement === first) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first.focus();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("keydown", onKey);
      returnFocusRef.current?.focus();
      returnFocusRef.current = null;
    };
  }, [open]);

  /** Everything behind Help is unreachable while it is open. */
  useEffect(() => {
    if (!open || !dialogRef.current) return;
    const shell = dialogRef.current.parentElement;
    if (!shell) return;
    const siblings = Array.from(shell.children).filter(
      (child): child is HTMLElement =>
        child !== dialogRef.current &&
        child instanceof HTMLElement &&
        child.dataset.modalLayer !== "consent",
    );
    const previous = siblings.map((element) => ({
      element,
      ariaHidden: element.getAttribute("aria-hidden"),
      inert: element.hasAttribute("inert"),
    }));
    siblings.forEach((element) => {
      element.setAttribute("inert", "");
      element.setAttribute("aria-hidden", "true");
    });
    return () => {
      previous.forEach(({ element, ariaHidden, inert }) => {
        if (inert) element.setAttribute("inert", "");
        else element.removeAttribute("inert");
        if (ariaHidden === null) element.removeAttribute("aria-hidden");
        else element.setAttribute("aria-hidden", ariaHidden);
      });
    };
  }, [open]);

  const commitActive = useCallback(
    (index: number) => {
      const candidate = listed[index];
      if (!candidate) return;
      setActiveIndex(index);
      setSelectedId(candidate.chunkId);
    },
    [listed],
  );

  /**
   * Combobox keys, per the listbox pattern.
   *
   * Options are not tab stops: focus stays in the field the reader is typing
   * in, and the active option is announced through `aria-activedescendant`.
   * Escape is deliberately not bound here — it belongs to the dialog, and
   * taking it would strand a keyboard user inside the search box.
   */
  const onSearchKeyDown = (event: React.KeyboardEvent<HTMLInputElement>) => {
    if (listed.length === 0) return;
    const last = listed.length - 1;
    if (event.key === "ArrowDown") {
      event.preventDefault();
      setActiveIndex((index) => (index >= last ? 0 : index + 1));
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      setActiveIndex((index) => (index <= 0 ? last : index - 1));
    } else if (event.key === "Home") {
      event.preventDefault();
      setActiveIndex(0);
    } else if (event.key === "End") {
      event.preventDefault();
      setActiveIndex(last);
    } else if (event.key === "Enter") {
      event.preventDefault();
      commitActive(activeIndex);
    }
  };

  useEffect(() => {
    if (!open) return;
    const active = listed[activeIndex];
    if (!active || !listRef.current) return;
    const option = listRef.current.querySelector<HTMLElement>(
      `[data-chunk-id="${CSS.escape(active.chunkId)}"]`,
    );
    // Keeping the active option visible is a convenience, not a guarantee:
    // some hosts have no scrollIntoView, and a missing scroll must never break
    // the keyboard navigation it was meant to assist.
    if (option && typeof option.scrollIntoView === "function") {
      option.scrollIntoView({ block: "nearest" });
    }
  }, [activeIndex, listed, open]);

  if (!open) return null;

  return (
    <div
      ref={dialogRef}
      className="help-center"
      data-modal-layer="help"
      data-help-status={view.status}
      role="dialog"
      aria-modal="true"
      aria-labelledby="help-center-title"
      aria-describedby="help-center-subtitle"
    >
      <div className="help-surface">
        <header className="help-header">
          <div>
            <p className="help-eyebrow">GrokPtah guidance</p>
            <h2 id="help-center-title">Help</h2>
            <p className="help-subtitle" id="help-center-subtitle">
              Search the documentation shipped in this build. Retrieval runs on this
              machine and cites what it returns.
            </p>
          </div>
          <button type="button" onClick={onClose} aria-label="Close Help">
            Close <span aria-hidden>Esc</span>
          </button>
        </header>

        <div className="help-layout">
          <aside className="help-nav" aria-label="Help search">
            <form className="help-search" role="search" onSubmit={(event) => event.preventDefault()}>
              <label htmlFor="help-search-input">Search help</label>
              <input
                id="help-search-input"
                role="combobox"
                aria-label="Search help"
                aria-expanded={listed.length > 0}
                aria-controls={listboxId}
                aria-autocomplete="list"
                aria-describedby="help-search-hint"
                aria-activedescendant={
                  listed[activeIndex] ? optionId(listed[activeIndex].chunkId) : undefined
                }
                autoComplete="off"
                value={query}
                onChange={(event) => setQuery(event.target.value)}
                onKeyDown={onSearchKeyDown}
                placeholder="Ask in your own words"
              />
            </form>
            <p className="sr-only" id="help-search-hint">
              Results update as you type. Use the up and down arrow keys to review them,
              Home and End to jump, and Enter to open the highlighted article. Escape
              closes Help.
            </p>

            <label className="help-topic-label" htmlFor="help-topic-filter">
              Topic
            </label>
            <select
              id="help-topic-filter"
              className="help-topic"
              aria-label="Topic"
              value={topic}
              onChange={(event) => setTopic(event.target.value as HelpTopic | "all")}
            >
              {TOPICS.map((item) => (
                <option value={item.value} key={item.value}>
                  {item.label}
                </option>
              ))}
            </select>

            <p className="help-result-count" aria-live="polite">
              {listed.length} {listed.length === 1 ? "result" : "results"}
            </p>
            <p className="help-retrieval-mode">
              Offline hybrid retrieval · no network, no model · corpus{" "}
              <span className="help-digest">{view.corpusDigest.slice(0, 12)}</span>
            </p>

            <ul
              className="help-list"
              id={listboxId}
              ref={listRef}
              role="listbox"
              aria-label={view.status === "browse" ? "Help articles" : "Help search results"}
            >
              {listed.map((candidate, index) => (
                <li
                  key={candidate.chunkId}
                  id={optionId(candidate.chunkId)}
                  data-chunk-id={candidate.chunkId}
                  role="option"
                  className={
                    "help-list-item" +
                    (candidate.chunkId === selected?.chunkId ? " is-selected" : "") +
                    (index === activeIndex ? " is-active" : "")
                  }
                  aria-selected={candidate.chunkId === selected?.chunkId}
                  onClick={() => commitActive(index)}
                >
                  <span className="help-list-topic">{topicLabel(candidate.topic)}</span>
                  <strong>{candidate.title}</strong>
                  <span>{candidate.summary}</span>
                  <span className="help-list-labels">
                    {candidate.chunkId === view.answer?.chunkId ? (
                      <span className="help-badge-answer">Answer</span>
                    ) : (
                      <span className="help-badge-suggestion">Suggestion</span>
                    )}
                  </span>
                  {candidate.matchedTerms.length > 0 && (
                    <small>
                      Matched {candidate.matchedTerms.slice(0, 3).join(", ")} · rank signal{" "}
                      {rankSignal(candidate.score)} — a ranking signal, not a certification
                    </small>
                  )}
                </li>
              ))}
            </ul>
          </aside>

          <p className="sr-only" role="status" aria-live="polite">
            {view.headline}. {selected ? `Showing ${selected.title}.` : "Nothing is shown."}
          </p>

          {/*
            The article pane scrolls, and at narrow widths it holds no
            focusable child, so without a tab stop a keyboard-only reader could
            not scroll it at all (WCAG 2.1.1). `tabIndex` makes it reachable
            and the label says what was reached.
          */}
          <article className="help-article" tabIndex={0} aria-label="Help article">
            <section
              className={`help-state help-state-${view.status}`}
              role={view.status === "rejected" ? "alert" : "status"}
              aria-label="Help retrieval outcome"
            >
              <strong className="help-state-headline">{view.headline}</strong>
              <p className="help-state-detail">{view.detail}</p>
              {view.abstainReason && (
                <p className="help-state-verdict">
                  Retriever verdict: <code>{view.abstainReason}</code>
                </p>
              )}
              {view.rejection && (
                <p className="help-state-verdict">
                  Rejected: <code>{view.rejection}</code>
                </p>
              )}
            </section>

            {selected ? (
              <>
                {!isPresentedAnswer && view.status !== "browse" && (
                  <p className="help-suggestion-note" role="note">
                    Shown as a suggestion. Help did not present this article as the answer
                    to “{view.query}”.
                  </p>
                )}
                <span className="help-article-topic">{topicLabel(selected.topic)}</span>
                <h3>{selected.title}</h3>
                <p className="help-article-summary">{selected.summary}</p>

                <section
                  className="help-citations"
                  aria-label={isPresentedAnswer ? "Cited answer" : "Match evidence"}
                >
                  <strong>
                    {isPresentedAnswer
                      ? "Why this article is the answer"
                      : "Why this article matched"}
                  </strong>
                  <p className="help-citation-note">
                    The quote below was re-read from the corpus before it was shown, and
                    names the documents backing that exact text.
                  </p>
                  <blockquote className="help-citation-quote">{selected.quote}</blockquote>
                  <ul aria-label="Sources">
                    {selected.citations.map((citation) => (
                      <li key={citation.sourceId} className="help-citation">
                        <code>{citation.path}</code> · {citation.heading}
                        <span className="help-citation-meta"> · verified</span>
                      </li>
                    ))}
                  </ul>
                  {selected.unverifiedCitationCount > 0 && (
                    <p className="help-citation-dropped" role="alert">
                      {selected.unverifiedCitationCount} source
                      {selected.unverifiedCitationCount === 1 ? " was" : "s were"} not shown:
                      the corpus did not reproduce them.
                    </p>
                  )}
                </section>

                <div className="help-source-card">
                  <strong>Source-backed offline guidance</strong>
                  <span>
                    {selected.articleId} · retrieval: {view.retrievalMode} · corpus digest{" "}
                    <code className="help-digest">{view.corpusDigest.slice(0, 12)}</code>
                  </span>
                  {view.query && (
                    <span>
                      Rank signal: {rankSignal(selected.score)}
                      <span className="help-confidence-note">
                        {" "}
                        · ranking signal only, not certification
                      </span>
                    </span>
                  )}
                </div>

                <section className="help-answer-card" aria-label="Written answer">
                  <strong>Ask for a written answer</strong>
                  {answersEnabled ? (
                    <p>
                      A written answer would be drafted from the cited articles above and
                      nothing else, and would be checked against the corpus before it was
                      shown.
                    </p>
                  ) : (
                    <p className="help-answers-disabled" role="note">
                      Not available in this build: the host's provider seam is disabled, so
                      no request can leave this machine. The cited documentation above is
                      the whole product, not a fallback.
                    </p>
                  )}
                  <p className="help-unknowns">
                    provider: {HELP_VIEW_UNKNOWNS.provider} · model: {HELP_VIEW_UNKNOWNS.model} ·
                    cost: {HELP_VIEW_UNKNOWNS.cost} · latency: {HELP_VIEW_UNKNOWNS.latency}
                  </p>
                  <p className="help-unknowns-note">{HELP_VIEW_UNKNOWNS.note}</p>
                </section>
              </>
            ) : (
              <div className="help-empty" role="status">
                <h3>No matching guidance</h3>
                <p>
                  {view.status === "rejected"
                    ? view.detail
                    : "Try a broader phrase, or clear the topic filter."}
                </p>
              </div>
            )}
          </article>
        </div>
      </div>
    </div>
  );
}
