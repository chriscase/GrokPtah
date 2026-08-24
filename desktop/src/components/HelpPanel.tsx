import { useEffect, useMemo, useRef, useState } from "react";
import { searchHelp, type HelpAudience, type HelpSearchHit } from "../lib/help";

export type HelpPanelProps = {
  open: boolean;
  onClose: () => void;
  /** The caller's already-authorized help audience. */
  audience?: HelpAudience;
  /** Restricted/operator entries are opt-in even for an operator audience. */
  includeRestricted?: boolean;
};

/** Accessible local Help Center; content is explanatory, never executable. */
export function HelpPanel({
  open,
  onClose,
  audience = "everyone",
  includeRestricted = false,
}: HelpPanelProps) {
  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState<HelpSearchHit | null>(null);
  const dialogRef = useRef<HTMLDivElement>(null);
  const queryRef = useRef<HTMLInputElement>(null);
  const restoreFocusRef = useRef<HTMLElement | null>(null);
  const hits = useMemo(
    () => (query.trim()
      ? searchHelp(query, {
          limit: 12,
          audience,
          includeRestricted: includeRestricted && audience !== "everyone",
        })
      : []),
    [audience, includeRestricted, query],
  );

  useEffect(() => {
    if (!open) return;
    restoreFocusRef.current = document.activeElement instanceof HTMLElement
      ? document.activeElement
      : null;
    queryRef.current?.focus();
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
      if (event.key !== "Tab") return;
      const focusable = dialogRef.current?.querySelectorAll<HTMLElement>(
        "button:not([disabled]), input:not([disabled]), [href], select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex=\"-1\"])",
      );
      if (!focusable || focusable.length === 0) return;
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
      restoreFocusRef.current?.focus();
      restoreFocusRef.current = null;
    };
  }, [open, onClose]);

  useEffect(() => {
    if (!open) {
      setQuery("");
      setSelected(null);
    }
  }, [open]);

  if (!open) return null;

  return (
    <div
      ref={dialogRef}
      className="help-panel"
      role="dialog"
      aria-modal="true"
      aria-labelledby="help-title"
      aria-describedby="help-description"
    >
      <header className="hp-header">
        <div>
          <h2 id="help-title">Help Center</h2>
          <p id="help-description">Search capabilities, approvals, recovery, Computer Use, and integrations.</p>
        </div>
        <button type="button" onClick={onClose} aria-label="Close Help Center">
          Close Esc
        </button>
      </header>
      <div className="hp-search-row">
        <label className="sr-only" htmlFor="help-query">Search Help Center</label>
        <input
          id="help-query"
          className="hp-query"
          ref={queryRef}
          value={query}
          onChange={(event) => {
            setQuery(event.target.value);
            setSelected(null);
          }}
          placeholder="Try ‘stale frame’, ‘gateway review’, or ‘restart agent’…"
        />
      </div>
      <div className="sr-only" role="status" aria-live="polite" aria-atomic="true">
        {query.trim() ? `${hits.length} help result${hits.length === 1 ? "" : "s"}` : ""}
      </div>
      <div className="hp-body">
        <section className="hp-results" aria-label="Help results">
          {!query.trim() && (
            <div className="hp-empty">Start with a question or a capability name.</div>
          )}
          {query.trim() && hits.length === 0 && (
            <div className="hp-empty">No matching help yet. Try a broader phrase.</div>
          )}
          <ul>
            {hits.map((hit) => (
              <li key={hit.entry.id}>
                <button
                  type="button"
                  className={`hp-result ${selected?.entry.id === hit.entry.id ? "active" : ""}`}
                  aria-pressed={selected?.entry.id === hit.entry.id}
                  onClick={() => setSelected(hit)}
                >
                  <strong>{hit.entry.title}</strong>
                  <span>{hit.entry.summary}</span>
                  <small>{hit.entry.tags.slice(0, 3).join(" · ")}</small>
                </button>
              </li>
            ))}
          </ul>
        </section>
        <article className="hp-article" aria-live="polite">
          {selected ? (
            <>
              <div className="hp-article-heading">
                <span className={`hp-access ${selected.entry.access}`}>
                  {selected.entry.access === "public" ? "General guidance" : "Gated guidance"}
                </span>
                <h3>{selected.entry.title}</h3>
              </div>
              <p>{selected.entry.body}</p>
              <div className="hp-tags">
                {selected.entry.tags.map((tag) => <span key={tag}>{tag}</span>)}
              </div>
              <p className="hp-boundary">
                Help explains behavior only. Before acting, re-check live capabilities,
                scope, approval, lease, and revision state.
              </p>
            </>
          ) : (
            <div className="hp-empty hp-article-empty">Select a result to read it here.</div>
          )}
        </article>
      </div>
    </div>
  );
}
