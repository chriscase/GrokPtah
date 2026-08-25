/**
 * Reusable React primitives for embedding Help retrieval.
 *
 * Deliberately unstyled beyond what accessibility requires: a consumer such as
 * ContextDesk brings its own visual language. Every primitive is driven by the
 * headless controller, so the same behavior is available without React.
 *
 * Three rules hold throughout:
 *   1. Nothing is ever rendered as HTML. Highlights are offset ranges applied
 *      to plain text, so provider or corpus text cannot inject markup.
 *   2. Layout uses relative units and wraps, so a 200% text zoom reflows
 *      rather than clipping.
 *   3. State is conveyed by ARIA and text, never by color alone, so forced-
 *      colors and high-contrast modes lose no information.
 */
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  createHelpSearchController,
  describeHelpResultForAssistiveTech,
  type HelpSearchController,
  type HelpSearchState,
} from "../consumer";
import type { HelpExcerpt } from "../retrieval/highlight";
import type { HelpCitation, HelpRetrievalOptions, HelpRetrievalResult } from "../retrieval/hybrid";

/** Subscribe to a Help search controller; creates one when not supplied. */
export function useHelpSearch(
  options: HelpRetrievalOptions = {},
  provided?: HelpSearchController,
): { state: HelpSearchState; controller: HelpSearchController } {
  const optionsRef = useRef(options);
  optionsRef.current = options;

  const controller = useMemo(
    () => provided ?? createHelpSearchController(optionsRef.current),
    [provided],
  );
  const [state, setState] = useState<HelpSearchState>(() => controller.getState());

  useEffect(() => {
    setState(controller.getState());
    const unsubscribe = controller.subscribe(setState);
    return () => {
      unsubscribe();
      // Only dispose a controller this hook owns; a supplied one outlives it.
      if (!provided) controller.dispose();
    };
  }, [controller, provided]);

  return { state, controller };
}

export type HelpHighlightedTextProps = {
  readonly excerpt: HelpExcerpt;
  /** Element used for highlighted spans. Defaults to `mark`. */
  readonly markAs?: "mark" | "strong" | "span";
  readonly className?: string;
  readonly markClassName?: string;
};

/**
 * Render an excerpt with its highlight ranges.
 *
 * Slices plain text into React children — there is no HTML string anywhere in
 * this path, so a crafted excerpt cannot introduce markup. `<mark>` carries
 * the emphasis semantically, which survives forced-colors mode where a
 * background tint would not.
 */
export function HelpHighlightedText({
  excerpt,
  markAs = "mark",
  className,
  markClassName,
}: HelpHighlightedTextProps): JSX.Element {
  const Mark = markAs;
  const parts: JSX.Element[] = [];
  let cursor = 0;
  excerpt.highlights.forEach((highlight, index) => {
    const start = Math.max(cursor, Math.min(highlight.start, excerpt.text.length));
    const end = Math.max(start, Math.min(start + highlight.length, excerpt.text.length));
    if (start > cursor) {
      parts.push(<span key={`t${index}`}>{excerpt.text.slice(cursor, start)}</span>);
    }
    if (end > start) {
      parts.push(
        <Mark key={`m${index}`} className={markClassName}>
          {excerpt.text.slice(start, end)}
        </Mark>,
      );
    }
    cursor = end;
  });
  if (cursor < excerpt.text.length) {
    parts.push(<span key="tail">{excerpt.text.slice(cursor)}</span>);
  }
  return <span className={className}>{parts}</span>;
}

export type HelpCitationListProps = {
  readonly citations: readonly HelpCitation[];
  readonly className?: string;
  /** Build an href for a source anchor. Omit to render non-interactive text. */
  readonly hrefFor?: (citation: HelpCitation) => string | undefined;
};

/**
 * Render citations as a labelled list.
 *
 * A list rather than inline text so assistive technology can announce the
 * count and step through entries. The heading is part of the visible label,
 * not a tooltip, so it survives at 200% zoom.
 */
export function HelpCitationList({
  citations,
  className,
  hrefFor,
}: HelpCitationListProps): JSX.Element | null {
  if (citations.length === 0) return null;
  return (
    <ul className={className} aria-label={`Sources (${citations.length})`}>
      {citations.map((citation) => {
        const label = `${citation.path} — ${citation.heading}`;
        const href = hrefFor?.(citation);
        return (
          <li key={`${citation.sourceId}:${citation.chunkId}`}>
            {href ? (
              <a href={href}>{label}</a>
            ) : (
              <span>{label}</span>
            )}
          </li>
        );
      })}
    </ul>
  );
}

export type HelpResultItemProps = {
  readonly result: HelpRetrievalResult;
  readonly total: number;
  readonly active?: boolean;
  readonly onActivate?: (result: HelpRetrievalResult) => void;
  readonly className?: string;
  readonly hrefFor?: (citation: HelpCitation) => string | undefined;
  /** Show the score breakdown. Off by default; useful for debugging. */
  readonly showScoreComponents?: boolean;
};

/** One result: title, excerpt with highlights, citations, and provenance. */
export function HelpResultItem({
  result,
  total,
  active = false,
  onActivate,
  className,
  hrefFor,
  showScoreComponents = false,
}: HelpResultItemProps): JSX.Element {
  return (
    <li
      className={className}
      role="option"
      aria-selected={active}
      aria-label={describeHelpResultForAssistiveTech(result, total)}
      data-active={active ? "true" : "false"}
      data-topic={result.topic}
      data-article-id={result.articleId}
      data-chunk-id={result.chunkId}
      onClick={onActivate ? () => onActivate(result) : undefined}
    >
      <span data-help-part="title">{result.title}</span>
      {/* Topic is text, not a colored dot: forced-colors keeps the meaning. */}
      <span data-help-part="topic">{result.topic.replace(/-/g, " ")}</span>
      <HelpHighlightedText excerpt={result.excerpt} />
      <HelpCitationList citations={result.citations} hrefFor={hrefFor} />
      {showScoreComponents ? (
        <span data-help-part="explanation">{result.explanation}</span>
      ) : null}
    </li>
  );
}

export type HelpResultsProps = {
  readonly state: HelpSearchState;
  readonly controller?: HelpSearchController;
  readonly onActivate?: (result: HelpRetrievalResult) => void;
  readonly className?: string;
  readonly itemClassName?: string;
  readonly hrefFor?: (citation: HelpCitation) => string | undefined;
  readonly showScoreComponents?: boolean;
  /** Message shown when retrieval declines to answer. */
  readonly abstainMessage?: (state: HelpSearchState) => string;
};

function defaultAbstainMessage(state: HelpSearchState): string {
  switch (state.abstentionReason) {
    case "below-confidence":
      return "No confident match in Help. Try different words, or rephrase the question.";
    case "no-match":
      return "No Help article matches that.";
    case "empty-query":
      return "Type a question to search Help.";
    case "cancelled":
      return "Search cancelled.";
    default:
      return "No results.";
  }
}

/**
 * The results list, with a live region for state changes.
 *
 * `aria-live="polite"` on the status line means a screen reader hears the
 * result count, any spelling correction, and any abstention without the
 * consumer wiring announcements itself.
 */
export function HelpResults({
  state,
  controller,
  onActivate,
  className,
  itemClassName,
  hrefFor,
  showScoreComponents = false,
  abstainMessage = defaultAbstainMessage,
}: HelpResultsProps): JSX.Element {
  const handleKeyDown = useCallback(
    (event: React.KeyboardEvent<HTMLUListElement>) => {
      if (!controller) return;
      if (event.key === "ArrowDown") {
        event.preventDefault();
        controller.moveActive(1);
      } else if (event.key === "ArrowUp") {
        event.preventDefault();
        controller.moveActive(-1);
      } else if (event.key === "Home") {
        event.preventDefault();
        controller.setActiveIndex(0);
      } else if (event.key === "End") {
        event.preventDefault();
        controller.setActiveIndex(state.results.length - 1);
      } else if ((event.key === "Enter" || event.key === " ") && state.activeIndex >= 0) {
        const result = state.results[state.activeIndex];
        if (result && onActivate) {
          event.preventDefault();
          onActivate(result);
        }
      }
    },
    [controller, onActivate, state.activeIndex, state.results],
  );

  const status = state.abstained
    ? abstainMessage(state)
    : `${state.results.length} Help ${state.results.length === 1 ? "result" : "results"}.`;
  const correction =
    state.corrections.length > 0
      ? ` Showing results for ${state.corrections.map((entry) => entry.to).join(", ")}.`
      : "";
  const redaction = state.redactedQuery
    ? " A credential in your query was removed before searching and was not sent anywhere."
    : "";

  return (
    <div className={className}>
      <p role="status" aria-live="polite" data-help-part="status">
        {`${status}${correction}${redaction}`}
      </p>
      {state.results.length > 0 ? (
        <ul
          role="listbox"
          aria-label="Help results"
          tabIndex={0}
          onKeyDown={handleKeyDown}
          data-help-part="results"
        >
          {state.results.map((result, index) => (
            <HelpResultItem
              key={result.chunkId}
              result={result}
              total={state.results.length}
              active={index === state.activeIndex}
              onActivate={onActivate}
              className={itemClassName}
              hrefFor={hrefFor}
              showScoreComponents={showScoreComponents}
            />
          ))}
        </ul>
      ) : null}
    </div>
  );
}

export type HelpSearchInputProps = {
  readonly controller: HelpSearchController;
  readonly state: HelpSearchState;
  readonly label?: string;
  readonly placeholder?: string;
  readonly className?: string;
  readonly id?: string;
};

/** A labelled search input bound to the controller. */
export function HelpSearchInput({
  controller,
  state,
  label = "Search Help",
  placeholder,
  className,
  id = "grokptah-help-search",
}: HelpSearchInputProps): JSX.Element {
  const [value, setValue] = useState(state.query);
  return (
    <div className={className}>
      {/* A real label, not a placeholder: placeholders vanish on input and
          are invisible to some assistive technology. */}
      <label htmlFor={id}>{label}</label>
      <input
        id={id}
        type="search"
        value={value}
        placeholder={placeholder}
        autoComplete="off"
        spellCheck={false}
        aria-describedby={`${id}-status`}
        onChange={(event) => {
          setValue(event.target.value);
          if (event.target.value.trim().length === 0) controller.clear();
          else controller.search(event.target.value);
        }}
        onKeyDown={(event) => {
          if (event.key === "Escape") {
            setValue("");
            controller.clear();
          }
        }}
      />
      <span id={`${id}-status`} data-help-part="input-status">
        {state.abstained && state.abstentionReason === "below-confidence"
          ? "No confident match"
          : ""}
      </span>
    </div>
  );
}
