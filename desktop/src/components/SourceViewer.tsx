import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { highlightLines } from "../lib/sourceHighlight";
import {
  matchIndexAtOrAfter,
  searchLines,
  searchStatus,
  segmentLine,
  stepMatch,
  type SourceMatch,
} from "../lib/sourceSearch";
import {
  rootIdentityLabel,
  sourceViewErrorSummary,
  truncationNotice,
  type SourceDocument,
} from "../lib/sourceView";

/** How many lines are rendered before the reader asks for more. */
const PAGE = 400;
/** Lines of lead-in kept above a requested line. */
const LEAD_IN = 40;

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
  /** The loaded document, or null while loading or after a refusal. */
  document: SourceDocument | null;
  /** Refusal from the boundary, shown with its plain-language summary. */
  error?: unknown;
  loading?: boolean;
  /** 1-based line to reveal on open, from a tool, test, or diff path. */
  initialLine?: number | null;
  onClose: () => void;
  /** Copy hook, injected so tests do not need a clipboard. */
  onCopy?: (text: string) => Promise<void>;
  onRetry?: () => void;
};

function encodingNotice(document: SourceDocument): string | null {
  if (document.encoding === "binary") {
    return "This is a binary file. Its bytes are not shown as text.";
  }
  if (document.encoding === "utf8_lossy") {
    return "Some bytes are not valid UTF-8 and are shown as the replacement character.";
  }
  return null;
}

async function defaultCopy(text: string): Promise<void> {
  await navigator.clipboard.writeText(text);
}

/**
 * Read-only source viewer.
 *
 * Opening a file no longer means asking a model to read it: this renders the
 * bytes the boundary returned, with real line numbers, in-file search, and an
 * identity strip naming the exact tree the file came from.
 *
 * Long files are paged rather than virtualised — an explicit "show more"
 * button keeps every rendered line reachable by keyboard and by a screen
 * reader, which scroll-driven virtualisation does not.
 */
export function SourceViewer({
  open,
  document: doc,
  error,
  loading = false,
  initialLine = null,
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
  const [visibleTo, setVisibleTo] = useState(PAGE);
  const [visibleFrom, setVisibleFrom] = useState(1);
  const [announcement, setAnnouncement] = useState("");
  const [copyState, setCopyState] = useState<"idle" | "copied" | "failed">("idle");

  const lines = doc?.lines ?? [];
  const anchor = initialLine && initialLine > 0 ? initialLine : 1;

  // Reset the window and search whenever a different document is shown.
  const documentKey = doc ? `${doc.rootId}:${doc.relativePath}:${doc.contentFingerprint}` : "";
  useEffect(() => {
    const from = Math.max(1, anchor - LEAD_IN);
    setVisibleFrom(from);
    setVisibleTo(Math.max(from + PAGE - 1, anchor));
    setMatchIndex(-1);
    setCopyState("idle");
  }, [documentKey, anchor]);

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

  const visibleLines = useMemo(
    () => lines.filter((line) => line.number >= visibleFrom && line.number <= visibleTo),
    [lines, visibleFrom, visibleTo],
  );

  const tokenRows = useMemo(
    () => (doc ? highlightLines(visibleLines.map((line) => line.text), doc.language) : []),
    [doc, visibleLines],
  );

  const activeMatch = matchIndex >= 0 ? matches[matchIndex] : undefined;

  /** Bring a line into the rendered window and scroll to it. */
  const reveal = useCallback(
    (line: number) => {
      setVisibleFrom((from) => Math.min(from, Math.max(1, line - LEAD_IN)));
      setVisibleTo((to) => Math.max(to, line + LEAD_IN));
    },
    [],
  );

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
      reveal(matches[next].line);
      setAnnouncement(searchStatus(matches.length, next, query));
    },
    [matches, matchIndex, query, anchor, reveal],
  );

  /**
   * Remember what had focus *before* anything inside the dialog takes it.
   *
   * This has to be a layout effect declared ahead of the one that moves
   * focus: React runs layout effects in order, so capturing later would
   * record the viewer's own code region and "restore" focus to a node that
   * no longer exists.
   */
  useLayoutEffect(() => {
    if (!open) return;
    const active = window.document.activeElement;
    returnFocusRef.current = active instanceof HTMLElement ? active : null;
    return () => {
      returnFocusRef.current?.focus();
      returnFocusRef.current = null;
    };
  }, [open]);

  // Keyboard handling: Escape closes, the find shortcut reaches search, Tab
  // stays inside the dialog.
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

  // Announce where the viewer landed, so a screen reader is not silent.
  useEffect(() => {
    if (!open || !doc) return;
    const total = doc.lineCount;
    setAnnouncement(
      initialLine && initialLine > 0
        ? `${doc.relativePath}, line ${initialLine} of ${total}`
        : `${doc.relativePath}, ${total} line${total === 1 ? "" : "s"}`,
    );
  }, [open, doc, initialLine]);

  if (!open) return null;

  const hasError = error !== undefined && error !== null;
  const notice = doc ? truncationNotice(doc) : null;
  const encoding = doc ? encodingNotice(doc) : null;
  const title = doc ? doc.relativePath : "Source";
  const remainingBelow = doc ? Math.max(0, Math.min(doc.lines.length, doc.lineCount) - visibleTo) : 0;
  const remainingAbove = visibleFrom - 1;

  async function copyAll() {
    if (!doc) return;
    const text = doc.lines.map((line) => line.text).join("\n");
    try {
      await (onCopy ?? defaultCopy)(text);
      setCopyState("copied");
      setAnnouncement(`Copied ${doc.lines.length} lines of ${doc.relativePath}`);
    } catch {
      setCopyState("failed");
      setAnnouncement("Copy failed. Select the text and copy it manually.");
    }
  }

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
            <h2 id="source-viewer-title" className="source-viewer-title" title={title}>
              {title}
            </h2>
            <p
              id="source-viewer-identity"
              className="source-viewer-identity"
              data-testid="source-viewer-identity"
            >
              {doc ? rootIdentityLabel(doc) : "Reading from the approved workspace"}
            </p>
          </div>
          <div className="source-viewer-actions">
            <button
              type="button"
              className="composer-chip quiet"
              onClick={() => void copyAll()}
              disabled={!doc || doc.encoding === "binary"}
              data-testid="source-viewer-copy"
            >
              {copyState === "copied" ? "Copied" : copyState === "failed" ? "Copy failed" : "Copy"}
            </button>
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
              placeholder="Search in file"
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

        {(notice || encoding) && (
          <div className="source-viewer-notice" role="status" data-testid="source-viewer-notice">
            {[encoding, notice].filter(Boolean).join(" · ")}
          </div>
        )}

        {hasError && (
          <div className="run-error source-viewer-error" role="alert" data-testid="source-viewer-error">
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

        {doc && doc.encoding === "binary" && (
          <div className="panel-block source-viewer-binary" data-testid="source-viewer-binary">
            {doc.byteLen} bytes of binary content. Nothing is rendered as text.
          </div>
        )}

        {doc && doc.encoding !== "binary" && (
          <>
            {remainingAbove > 0 && (
              <button
                type="button"
                className="composer-chip quiet source-viewer-more"
                data-testid="source-viewer-show-earlier"
                onClick={() => setVisibleFrom((from) => Math.max(1, from - PAGE))}
              >
                Show {Math.min(PAGE, remainingAbove)} earlier lines
              </button>
            )}
            <div
              className="source-viewer-code"
              ref={codeRef}
              tabIndex={0}
              role="group"
              aria-label={`${doc.relativePath}, ${doc.lineCount} lines, read only`}
              data-testid="source-viewer-code"
            >
              {/* `role="list"` is restated because the stylesheet removes
                  list markers, which drops list semantics in some browsers. */}
              <ol className="source-viewer-lines" role="list">
                {visibleLines.map((line, index) => {
                  const lineMatches = matchesByLine.get(line.number) ?? [];
                  const isActive = activeMatch?.line === line.number;
                  return (
                    <li
                      key={line.number}
                      value={line.number}
                      id={`source-line-${line.number}`}
                      className={`source-viewer-line${isActive ? " is-active-match" : ""}`}
                      data-line={line.number}
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
                          ? segmentLine(line.text, lineMatches).map((segment, part) =>
                              segment.matched ? (
                                <mark key={part} className="source-viewer-match">
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
            {remainingBelow > 0 && (
              <button
                type="button"
                className="composer-chip quiet source-viewer-more"
                data-testid="source-viewer-show-more"
                onClick={() => setVisibleTo((to) => to + PAGE)}
              >
                Show {Math.min(PAGE, remainingBelow)} more lines
              </button>
            )}
          </>
        )}

        <p className="sr-only" role="status" aria-live="polite" data-testid="source-viewer-live">
          {announcement}
        </p>
      </div>
    </div>
  );
}
