import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { highlightLines } from "../lib/sourceHighlight";
import {
  matchIndexAtOrAfter,
  rangePosition,
  searchLines,
  searchStatus,
  segmentLine,
  stepMatch,
  type RangeHighlight,
  type SourceMatch,
} from "../lib/sourceSearch";
import {
  projectionNotice,
  readProgress,
  rootIdentityLabel,
  sourceViewErrorSummary,
  type SourceDocument,
  type SourceLine,
  type SourceRootDescriptor,
} from "../lib/sourceView";

const FOCUSABLE_SELECTOR =
  'button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), a[href], [tabindex]:not([tabindex="-1"])';

function focusableIn(root: HTMLElement | null): HTMLElement[] {
  if (!root) return [];
  return Array.from(root.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR)).filter(
    (element) => !element.closest("[inert]"),
  );
}

export type SourceViewerProps = {
  open: boolean;
  /** The most recent page. Carries identity, limits, and classification. */
  document: SourceDocument | null;
  /** Every line loaded so far, with continued lines already rejoined. */
  lines: SourceLine[];
  error?: unknown;
  loading?: boolean;
  loadingMore?: boolean;
  hasMore?: boolean;
  /** 1-based line to reveal on open, from a tool, test, or diff path. */
  initialLine?: number | null;
  /** Optional multi-line range to highlight, e.g. a diff hunk. */
  highlightRange?: RangeHighlight | null;
  /** Present when the request matched more than one approved root. */
  rootChoice?: { candidates: SourceRootDescriptor[] } | null;
  onChooseRoot?: (token: string) => void;
  onLoadMore?: () => void;
  onClose: () => void;
  /** Copy hook, injected so tests do not need a clipboard. */
  onCopy?: (text: string) => Promise<void>;
  onRetry?: () => void;
};

async function defaultCopy(text: string): Promise<void> {
  await navigator.clipboard.writeText(text);
}

/**
 * Read-only source viewer.
 *
 * Renders the bytes the boundary returned, with real line numbers, in-file
 * search, a range highlight for the location that was opened, and an identity
 * strip naming the exact tree — by kind, label, and digest, never by path.
 *
 * The dialog makes the rest of the app inert while it is open, contains and
 * restores focus, and pages with an explicit control rather than
 * scroll-driven virtualisation so every line stays reachable by keyboard and
 * by a screen reader.
 */
export function SourceViewer({
  open,
  document: doc,
  lines,
  error,
  loading = false,
  loadingMore = false,
  hasMore = false,
  initialLine = null,
  highlightRange = null,
  rootChoice = null,
  onChooseRoot,
  onLoadMore,
  onClose,
  onCopy,
  onRetry,
}: SourceViewerProps) {
  const dialogRef = useRef<HTMLDivElement | null>(null);
  const codeRef = useRef<HTMLDivElement | null>(null);
  const searchRef = useRef<HTMLInputElement | null>(null);
  const returnFocusRef = useRef<HTMLElement | null>(null);
  const onCloseRef = useRef(onClose);
  onCloseRef.current = onClose;

  const [query, setQuery] = useState("");
  const [caseSensitive, setCaseSensitive] = useState(false);
  const [wholeWord, setWholeWord] = useState(false);
  const [matchIndex, setMatchIndex] = useState(-1);
  const [announcement, setAnnouncement] = useState("");
  const [copyState, setCopyState] = useState<"idle" | "copied" | "failed">("idle");

  const anchor = initialLine && initialLine > 0 ? initialLine : 1;
  const documentKey = doc ? `${doc.root.token}:${doc.relativePath}:${doc.identity.digest}` : "";

  useEffect(() => {
    setMatchIndex(-1);
    setCopyState("idle");
  }, [documentKey]);

  const matches = useMemo<SourceMatch[]>(
    () => searchLines(lines, query, { caseSensitive, wholeWord }),
    [lines, query, caseSensitive, wholeWord],
  );

  const matchesByLine = useMemo(() => {
    const grouped = new Map<number, SourceMatch[]>();
    for (const match of matches) {
      const existing = grouped.get(match.line);
      if (existing) existing.push(match);
      else grouped.set(match.line, [match]);
    }
    return grouped;
  }, [matches]);

  const tokenRows = useMemo(
    () => (doc ? highlightLines(lines.map((line) => line.text), doc.language) : []),
    [doc, lines],
  );

  const activeMatch = matchIndex >= 0 ? matches[matchIndex] : undefined;

  const goToMatch = useCallback(
    (delta: number) => {
      if (matches.length === 0) {
        setAnnouncement(searchStatus(0, -1, query));
        return;
      }
      const next =
        matchIndex < 0 && delta > 0
          ? matchIndexAtOrAfter(matches, anchor)
          : stepMatch(matches.length, matchIndex, delta);
      setMatchIndex(next);
      setAnnouncement(searchStatus(matches.length, next, query));
      const target = window.document.getElementById(`source-line-${matches[next].line}`);
      target?.scrollIntoView({ block: "center", behavior: "auto" });
    },
    [matches, matchIndex, query, anchor],
  );

  /**
   * Remember what had focus *before* anything inside the dialog takes it, and
   * make the rest of the page inert while it is open.
   *
   * This has to be a layout effect declared ahead of the one that moves focus:
   * React runs layout effects in order, so capturing later would record the
   * viewer's own code region and "restore" focus to a node that no longer
   * exists.
   */
  useLayoutEffect(() => {
    if (!open) return;
    const active = window.document.activeElement;
    returnFocusRef.current = active instanceof HTMLElement ? active : null;

    // Everything outside the dialog becomes inert, so a screen reader cannot
    // wander into the application behind an open modal.
    //
    // The walk goes up the dialog's own ancestor path and inerts the siblings
    // at each level. Inerting the direct children of `body` instead would mark
    // the app root inert — and the dialog lives inside it, so the modal would
    // make itself unreachable.
    const restored: Array<{ element: HTMLElement; inert: boolean; hidden: string | null }> = [];
    let node: HTMLElement | null = dialogRef.current;
    while (node && node !== window.document.body) {
      const parent: HTMLElement | null = node.parentElement;
      if (!parent) break;
      for (const child of Array.from(parent.children)) {
        if (child === node || !(child instanceof HTMLElement)) continue;
        restored.push({
          element: child,
          inert: child.hasAttribute("inert"),
          hidden: child.getAttribute("aria-hidden"),
        });
        child.setAttribute("inert", "");
        child.setAttribute("aria-hidden", "true");
      }
      node = parent;
    }

    return () => {
      for (const entry of restored) {
        if (!entry.inert) entry.element.removeAttribute("inert");
        if (entry.hidden === null) entry.element.removeAttribute("aria-hidden");
        else entry.element.setAttribute("aria-hidden", entry.hidden);
      }
      returnFocusRef.current?.focus();
      returnFocusRef.current = null;
    };
  }, [open]);

  // Keyboard: Escape closes, the find shortcut reaches search, Tab is trapped.
  useEffect(() => {
    if (!open) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        onCloseRef.current();
        return;
      }
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "f") {
        event.preventDefault();
        searchRef.current?.focus();
        searchRef.current?.select();
        return;
      }
      if (event.key !== "Tab") return;
      const focusable = focusableIn(dialogRef.current);
      if (focusable.length === 0) return;
      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      if (event.shiftKey && window.document.activeElement === first) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && window.document.activeElement === last) {
        event.preventDefault();
        first.focus();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open]);

  // Land focus inside the dialog once there is something to read.
  useLayoutEffect(() => {
    if (!open) return;
    const target =
      codeRef.current ??
      dialogRef.current?.querySelector<HTMLElement>(FOCUSABLE_SELECTOR) ??
      null;
    target?.focus();
  }, [open, documentKey, loading]);

  // Scroll the opened line into view once it exists.
  useLayoutEffect(() => {
    if (!open || !doc || anchor <= 1) return;
    const target = window.document.getElementById(`source-line-${anchor}`);
    target?.scrollIntoView({ block: "center", behavior: "auto" });
  }, [open, doc, anchor, lines.length]);

  // Announce where the viewer landed, so a screen reader is not silent.
  useEffect(() => {
    if (!open || !doc) return;
    setAnnouncement(
      initialLine && initialLine > 0
        ? `${doc.relativePath}, line ${initialLine}, ${readProgress(doc, lines.length)}`
        : `${doc.relativePath}, ${readProgress(doc, lines.length)}`,
    );
  }, [open, doc, initialLine, lines.length]);

  if (!open) return null;

  const hasError = error !== undefined && error !== null;
  const notice = doc ? projectionNotice(doc) : null;
  const title = doc ? doc.relativePath : "Source";
  const isBinary = doc?.content.verdict === "binary";

  async function copyText(text: string, description: string) {
    try {
      await (onCopy ?? defaultCopy)(text);
      setCopyState("copied");
      setAnnouncement(`Copied ${description}`);
    } catch {
      setCopyState("failed");
      setAnnouncement("Copy failed. Select the text and copy it manually.");
    }
  }

  const loadedText = lines.map((line) => line.text).join("\n");
  const rangeLines =
    highlightRange === null
      ? []
      : lines.filter((line) => rangePosition(highlightRange, line.number) !== "outside");

  return (
    <div className="modal-backdrop source-viewer-backdrop" data-modal-layer="source-viewer">
      <div
        className="modal source-viewer"
        role="dialog"
        aria-modal="true"
        aria-labelledby="source-viewer-title"
        aria-describedby="source-viewer-identity"
        data-testid="source-viewer"
        ref={dialogRef}
      >
        <div className="source-viewer-header">
          <div className="source-viewer-heading">
            <h2 id="source-viewer-title" className="source-viewer-title">
              {title}
            </h2>
            <p
              id="source-viewer-identity"
              className="source-viewer-identity"
              data-testid="source-viewer-identity"
            >
              {doc ? rootIdentityLabel(doc) : "Waiting for an approved workspace"}
            </p>
          </div>
          <div className="source-viewer-actions">
            <button
              type="button"
              className="composer-chip quiet"
              onClick={() => void copyText(loadedText, `${lines.length} loaded lines of ${title}`)}
              disabled={!doc || isBinary || lines.length === 0}
              data-testid="source-viewer-copy-loaded"
              title={
                doc && !doc.chunk.eof
                  ? "Copies only the lines loaded so far, not the whole file"
                  : "Copies the whole file"
              }
            >
              {copyState === "copied"
                ? "Copied"
                : copyState === "failed"
                  ? "Copy failed"
                  : doc && !doc.chunk.eof
                    ? `Copy ${lines.length} loaded lines`
                    : "Copy whole file"}
            </button>
            {rangeLines.length > 0 && (
              <button
                type="button"
                className="composer-chip quiet"
                data-testid="source-viewer-copy-range"
                onClick={() =>
                  void copyText(
                    rangeLines.map((line) => line.text).join("\n"),
                    `lines ${rangeLines[0].number} to ${rangeLines[rangeLines.length - 1].number}`,
                  )
                }
              >
                Copy lines {rangeLines[0].number}–{rangeLines[rangeLines.length - 1].number}
              </button>
            )}
            <button
              type="button"
              className="icon-btn"
              onClick={onClose}
              aria-label="Close source viewer"
              data-testid="source-viewer-close"
            >
              ✕
            </button>
          </div>
        </div>

        <div className="source-viewer-search" role="search">
          <label className="source-viewer-search-field">
            <span className="sr-only">Search in this file</span>
            <input
              ref={searchRef}
              type="text"
              value={query}
              placeholder="Search loaded lines"
              autoComplete="off"
              spellCheck={false}
              data-testid="source-viewer-search"
              onChange={(event) => {
                setQuery(event.target.value);
                setMatchIndex(-1);
              }}
              onKeyDown={(event) => {
                if (event.key !== "Enter") return;
                event.preventDefault();
                goToMatch(event.shiftKey ? -1 : 1);
              }}
            />
          </label>
          <span className="source-viewer-match-count" data-testid="source-viewer-match-count">
            {query ? `${matches.length ? matchIndex + 1 || 1 : 0}/${matches.length}` : ""}
          </span>
          <button
            type="button"
            className="composer-chip quiet"
            onClick={() => goToMatch(-1)}
            disabled={matches.length === 0}
            aria-label="Previous match"
          >
            ↑
          </button>
          <button
            type="button"
            className="composer-chip quiet"
            onClick={() => goToMatch(1)}
            disabled={matches.length === 0}
            aria-label="Next match"
          >
            ↓
          </button>
          <label className="source-viewer-toggle">
            <input
              type="checkbox"
              checked={caseSensitive}
              onChange={(event) => {
                setCaseSensitive(event.target.checked);
                setMatchIndex(-1);
              }}
            />
            <span>Match case</span>
          </label>
          <label className="source-viewer-toggle">
            <input
              type="checkbox"
              checked={wholeWord}
              onChange={(event) => {
                setWholeWord(event.target.checked);
                setMatchIndex(-1);
              }}
            />
            <span>Whole word</span>
          </label>
        </div>

        {rootChoice && rootChoice.candidates.length > 0 && (
          <div className="source-viewer-choice" role="group" aria-label="Choose a workspace">
            <p className="source-viewer-choice-prompt">
              More than one approved workspace matched. Choose the one you mean.
            </p>
            <ul className="source-viewer-choice-list">
              {rootChoice.candidates.map((candidate) => (
                <li key={candidate.token}>
                  <button
                    type="button"
                    className="composer-chip"
                    data-testid={`source-viewer-choice-${candidate.pathDigest.slice(0, 12)}`}
                    onClick={() => onChooseRoot?.(candidate.token)}
                  >
                    {candidate.kind === "isolated_worktree" ? "Isolated worktree" : "Workspace"} ·{" "}
                    {candidate.label} · {candidate.pathDigest.slice(0, 12)}
                  </button>
                </li>
              ))}
            </ul>
          </div>
        )}

        {notice && (
          <div className="source-viewer-notice" role="status" data-testid="source-viewer-notice">
            {notice}
          </div>
        )}

        {hasError && (
          <div
            className="run-error source-viewer-error"
            role="alert"
            data-testid="source-viewer-error"
          >
            <span>{sourceViewErrorSummary(error)}</span>
            {onRetry && (
              <button type="button" className="composer-chip quiet" onClick={onRetry}>
                Try again
              </button>
            )}
          </div>
        )}

        {loading && !doc && (
          <div className="panel-block source-viewer-loading" role="status">
            Reading file…
          </div>
        )}

        {doc && isBinary && (
          <div className="panel-block source-viewer-binary" data-testid="source-viewer-binary">
            {doc.byteLen} bytes of binary content. Nothing is rendered as text.
          </div>
        )}

        {doc && !isBinary && (
          <>
            <div
              className="source-viewer-code"
              ref={codeRef}
              tabIndex={0}
              role="group"
              aria-label={`${doc.relativePath}, ${lines.length} lines loaded, read only`}
              data-testid="source-viewer-code"
            >
              {/* `role="list"` is restated because the stylesheet removes list
                  markers, which drops list semantics in some browsers. */}
              <ol className="source-viewer-lines" role="list">
                {lines.map((line, index) => {
                  const lineMatches = matchesByLine.get(line.number) ?? [];
                  const isActive = activeMatch?.line === line.number;
                  const position = rangePosition(highlightRange, line.number);
                  return (
                    <li
                      key={line.number}
                      value={line.number}
                      id={`source-line-${line.number}`}
                      className={`source-viewer-line${isActive ? " is-active-match" : ""} range-${position}`}
                      data-line={line.number}
                      data-range={position}
                      data-testid={`source-line-${line.number}`}
                    >
                      <span className="source-viewer-line-number" aria-hidden="true">
                        {line.number}
                      </span>
                      <code className="source-viewer-line-text">
                        {/* A line with matches renders as search segments so
                            the hit is unmissable; syntax colour returns as
                            soon as the search is cleared. */}
                        {lineMatches.length > 0
                          ? segmentLine(line.text, lineMatches, activeMatch).map((segment, part) =>
                              segment.matched ? (
                                <mark
                                  key={part}
                                  className={`source-viewer-match${segment.active ? " is-active" : ""}`}
                                >
                                  {segment.text}
                                </mark>
                              ) : (
                                <span key={part}>{segment.text}</span>
                              ),
                            )
                          : (tokenRows[index] ?? []).map((token, part) => (
                              <span key={part} className={`tok-${token.kind}`}>
                                {token.text}
                              </span>
                            ))}
                        {line.truncated && (
                          <span className="source-viewer-line-cut" title="This line was truncated">
                            {" ⋯"}
                          </span>
                        )}
                      </code>
                    </li>
                  );
                })}
              </ol>
            </div>
            <div className="source-viewer-footer">
              <span className="source-viewer-progress" data-testid="source-viewer-progress">
                {readProgress(doc, lines.length)}
              </span>
              {hasMore && (
                <button
                  type="button"
                  className="composer-chip quiet source-viewer-more"
                  data-testid="source-viewer-load-more"
                  onClick={onLoadMore}
                  disabled={loadingMore}
                >
                  {loadingMore ? "Loading…" : "Load more lines"}
                </button>
              )}
            </div>
          </>
        )}

        <p
          className="sr-only"
          role="status"
          aria-live="polite"
          data-testid="source-viewer-live"
        >
          {announcement}
        </p>
      </div>
    </div>
  );
}
