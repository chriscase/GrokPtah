import { useEffect, useMemo, useState } from "react";
import { searchHelp, type HelpSearchHit } from "../lib/help";

export type HelpPanelProps = {
  open: boolean;
  onClose: () => void;
};

/** Accessible local Help Center; content is explanatory, never executable. */
export function HelpPanel({ open, onClose }: HelpPanelProps) {
  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState<HelpSearchHit | null>(null);
  const hits = useMemo(
    () => (query.trim() ? searchHelp(query, { limit: 12, includeRestricted: true }) : []),
    [query],
  );

  useEffect(() => {
    if (!open) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onClose]);

  useEffect(() => {
    if (!open) {
      setQuery("");
      setSelected(null);
    }
  }, [open]);

  if (!open) return null;

  return (
    <div className="help-panel" role="dialog" aria-modal="true" aria-label="Help Center">
      <header className="hp-header">
        <div>
          <h2>Help Center</h2>
          <p>Search capabilities, approvals, recovery, Computer Use, and integrations.</p>
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
          autoFocus
          value={query}
          onChange={(event) => {
            setQuery(event.target.value);
            setSelected(null);
          }}
          placeholder="Try ‘stale frame’, ‘gateway review’, or ‘restart agent’…"
        />
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
