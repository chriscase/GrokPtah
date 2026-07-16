import { useCallback, useEffect, useState } from "react";
import { api } from "../lib/api";
import type { SearchHit } from "../lib/protocol";

export type SearchPanelProps = {
  open: boolean;
  defaultKind?: "all" | "chat" | "build";
  onClose: () => void;
  onOpenSession: (sessionId: string, kind: string) => void;
};

/**
 * Hybrid search across Grok chats + build sessions (keyword + semantic TF-IDF).
 */
export function SearchPanel({
  open,
  defaultKind = "all",
  onClose,
  onOpenSession,
}: SearchPanelProps) {
  const [query, setQuery] = useState("");
  const [mode, setMode] = useState<"hybrid" | "keyword" | "semantic">("hybrid");
  const [kind, setKind] = useState<"all" | "chat" | "build">(defaultKind);
  const [includeArchived, setIncludeArchived] = useState(false);
  const [hits, setHits] = useState<SearchHit[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [searched, setSearched] = useState(false);

  useEffect(() => {
    if (open) setKind(defaultKind);
  }, [open, defaultKind]);

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onClose]);

  const runSearch = useCallback(async () => {
    const q = query.trim();
    if (!q) {
      setHits([]);
      setSearched(false);
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const res = await api.searchSessions({
        query: q,
        mode,
        kind,
        includeArchived,
        limit: 50,
      });
      setHits(res);
      setSearched(true);
    } catch (e) {
      setError(String(e));
      setHits([]);
    } finally {
      setBusy(false);
    }
  }, [query, mode, kind, includeArchived]);

  if (!open) return null;

  return (
    <div className="search-panel" role="dialog" aria-modal="true" aria-label="Search">
      <header className="sp-header">
        <div>
          <h2>Search</h2>
          <span className="sp-sub">
            Keyword · semantic (TF–IDF) · hybrid — chats & builds
          </span>
        </div>
        <button type="button" onClick={onClose}>
          Close Esc
        </button>
      </header>

      <form
        className="sp-form"
        onSubmit={(e) => {
          e.preventDefault();
          void runSearch();
        }}
      >
        <input
          className="sp-query"
          autoFocus
          placeholder="Search messages, titles, tags, folders…"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
        />
        <select
          value={mode}
          onChange={(e) =>
            setMode(e.target.value as "hybrid" | "keyword" | "semantic")
          }
          title="Ranking mode"
        >
          <option value="hybrid">Hybrid</option>
          <option value="keyword">Keyword</option>
          <option value="semantic">Semantic</option>
        </select>
        <select
          value={kind}
          onChange={(e) =>
            setKind(e.target.value as "all" | "chat" | "build")
          }
        >
          <option value="all">All kinds</option>
          <option value="chat">Chats only</option>
          <option value="build">Builds only</option>
        </select>
        <label className="sp-check">
          <input
            type="checkbox"
            checked={includeArchived}
            onChange={(e) => setIncludeArchived(e.target.checked)}
          />
          Archive
        </label>
        <button type="submit" className="primary" disabled={busy || !query.trim()}>
          {busy ? "Searching…" : "Search"}
        </button>
      </form>

      {error && <div className="sp-error">{error}</div>}

      <div className="sp-results">
        {!searched && !busy && (
          <div className="sp-empty">
            Search across Grok chats and coding build sessions. Hybrid mode
            blends exact keyword matches with semantic TF–IDF similarity.
          </div>
        )}
        {searched && hits.length === 0 && !busy && (
          <div className="sp-empty">No matches.</div>
        )}
        <ul className="sp-list">
          {hits.map((h) => (
            <li key={`${h.session_id}-${h.message_index ?? "m"}-${h.score}`}>
              <button
                type="button"
                className="sp-hit"
                onClick={() => onOpenSession(h.session_id, h.kind)}
              >
                <div className="sp-hit-top">
                  <span className={`sp-kind ${h.kind}`}>{h.kind}</span>
                  <strong>{h.title}</strong>
                  <span className="sp-score" title="hybrid / keyword / semantic">
                    {h.score.toFixed(2)}
                    <span className="sp-score-detail">
                      {" "}
                      · kw {h.keyword_score.toFixed(2)} · sem{" "}
                      {h.semantic_score.toFixed(2)}
                    </span>
                  </span>
                </div>
                <div className="sp-snippet">{h.snippet}</div>
                <div className="sp-hit-meta">
                  <span>{h.match_field}</span>
                  {h.folder && <span>· ▣ {h.folder}</span>}
                  {(h.tags ?? []).slice(0, 4).map((t) => (
                    <span key={t}>· #{t}</span>
                  ))}
                  {h.archived && <span>· archived</span>}
                </div>
              </button>
            </li>
          ))}
        </ul>
      </div>
    </div>
  );
}
