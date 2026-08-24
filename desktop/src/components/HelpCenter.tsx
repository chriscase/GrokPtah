import { useEffect, useMemo, useState } from "react";
import {
  buildHelpAssistantRequest,
  buildHelpSemanticRequest,
  HELP_ARTICLES,
  searchHelp,
  validateHelpAssistantAnswer,
  validateHelpSemanticAnswer,
  type HelpAssistantAnswer,
  type HelpAssistantRequest,
  type HelpSemanticAnswer,
  type HelpSemanticRequest,
  type HelpTopic,
} from "../lib/helpCenter";

export type HelpCenterProps = {
  open: boolean;
  onClose: () => void;
  /** Optional provider adapter; the UI still requires confirmation before it is called. */
  onAskAssistant?: (request: HelpAssistantRequest) => Promise<HelpAssistantAnswer>;
  /** Optional meaning-based ranking adapter; the UI requires confirmation before it is called. */
  onSearchSemantic?: (request: HelpSemanticRequest) => Promise<HelpSemanticAnswer>;
  assistantProviderLabel?: string;
};

const TOPICS: Array<{ value: HelpTopic | "all"; label: string }> = [
  { value: "all", label: "All topics" },
  { value: "getting-started", label: "Getting started" },
  { value: "providers", label: "Providers" },
  { value: "computer-use", label: "Computer Use" },
  { value: "operations", label: "Operations" },
];

type AssistantState =
  | { status: "idle" }
  | { status: "confirm"; request: HelpAssistantRequest }
  | { status: "loading" }
  | { status: "answer"; answer: HelpAssistantAnswer }
  | { status: "error"; message: string };

type SemanticState =
  | { status: "idle" }
  | { status: "confirm"; request: HelpSemanticRequest }
  | { status: "loading" }
  | { status: "results"; results: ReturnType<typeof searchHelp>; uncertainty: string }
  | { status: "error"; message: string };

export function HelpCenter({
  open,
  onClose,
  onAskAssistant,
  onSearchSemantic,
  assistantProviderLabel,
}: HelpCenterProps) {
  const [query, setQuery] = useState("");
  const [topic, setTopic] = useState<HelpTopic | "all">("all");
  const [selectedId, setSelectedId] = useState(HELP_ARTICLES[0]?.id ?? "");
  const [assistant, setAssistant] = useState<AssistantState>({ status: "idle" });
  const [semantic, setSemantic] = useState<SemanticState>({ status: "idle" });

  const lexicalResults = useMemo(() => {
    if (!query.trim()) {
      return HELP_ARTICLES.filter(
        (article) => topic === "all" || article.topic === topic,
      ).map((article) => ({
        article,
        score: 0,
        confidence: 0,
        matchedTerms: [],
        retrievalMode: "offline-lexical" as const,
      }));
    }
    return searchHelp(query, topic);
  }, [query, topic]);

  const results = semantic.status === "results" ? semantic.results : lexicalResults;
  const retrievalMode = semantic.status === "results"
    ? "provider-semantic"
    : "offline-lexical";

  const selectedResult =
    results.find((result) => result.article.id === selectedId) ??
    results[0] ??
    null;
  const selected = selectedResult?.article ?? null;

  useEffect(() => {
    if (!open) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onClose]);

  useEffect(() => {
    if (selected && !results.some((result) => result.article.id === selectedId)) {
      setSelectedId(selected.id);
    }
  }, [results, selected, selectedId]);

  useEffect(() => {
    setAssistant({ status: "idle" });
  }, [selectedId, query]);

  useEffect(() => {
    setSemantic({ status: "idle" });
  }, [query, topic]);

  const beginAssistantRequest = () => {
    if (!selected || !onAskAssistant) return;
    setAssistant({
      status: "confirm",
      request: buildHelpAssistantRequest(selected, query || selected.title, retrievalMode),
    });
  };

  const confirmAssistantRequest = async () => {
    if (assistant.status !== "confirm" || !onAskAssistant) return;
    setAssistant({ status: "loading" });
    try {
      const answer = await onAskAssistant(assistant.request);
      const validation = validateHelpAssistantAnswer(
        answer,
        assistant.request.sources.map((source) => source.id),
      );
      if (!validation.accepted) {
        setAssistant({ status: "error", message: `Assistant answer rejected: ${validation.reason}.` });
        return;
      }
      setAssistant({ status: "answer", answer });
    } catch {
      setAssistant({ status: "error", message: "Assistant unavailable; cited offline guidance remains authoritative." });
    }
  };

  const beginSemanticRequest = () => {
    const trimmed = query.trim();
    if (!trimmed || !onSearchSemantic) return;
    setSemantic({
      status: "confirm",
      request: buildHelpSemanticRequest(trimmed),
    });
  };

  const confirmSemanticRequest = async () => {
    if (semantic.status !== "confirm" || !onSearchSemantic) return;
    setSemantic({ status: "loading" });
    try {
      const answer = await onSearchSemantic(semantic.request);
      const validation = validateHelpSemanticAnswer(
        answer,
        semantic.request.candidates.map((candidate) => candidate.articleId),
      );
      if (!validation.accepted) {
        setSemantic({ status: "error", message: `Semantic ranking rejected: ${validation.reason}.` });
        return;
      }
      const byId = new Map(HELP_ARTICLES.map((article) => [article.id, article]));
      const ranked = answer.results
        .slice()
        .sort((a, b) => b.score - a.score)
        .map((result) => {
          const article = byId.get(result.articleId);
          if (!article) return null;
          return {
            article,
            score: result.score,
            confidence: result.score,
            matchedTerms: [],
            retrievalMode: "provider-semantic" as const,
          };
        })
        .filter((result): result is NonNullable<typeof result> => result !== null);
      setSemantic({ status: "results", results: ranked, uncertainty: answer.uncertainty });
    } catch {
      setSemantic({ status: "error", message: "Semantic search unavailable; offline lexical guidance remains available." });
    }
  };

  if (!open) return null;

  return (
    <div
      className="help-center"
      role="dialog"
      aria-modal="true"
      aria-labelledby="help-center-title"
      aria-describedby="help-center-subtitle"
    >
      <header className="help-header">
        <div>
          <p className="help-eyebrow">GrokPtah guidance</p>
          <h2 id="help-center-title">Help Center</h2>
          <p className="help-subtitle" id="help-center-subtitle">
            Search trustworthy product guidance without sending data to a model.
          </p>
        </div>
        <button type="button" onClick={onClose} aria-label="Close Help Center">
          Close <span aria-hidden>Esc</span>
        </button>
      </header>

      <div className="help-layout">
        <aside className="help-nav" aria-label="Help articles">
          <form
            className="help-search"
            onSubmit={(event) => event.preventDefault()}
          >
            <label htmlFor="help-search-input">Search help</label>
            <input
              id="help-search-input"
              aria-label="Search help"
              autoFocus
              value={query}
              onChange={(event) => setQuery(event.target.value)}
              placeholder="Try “quota”, “stale frame”, or “find a build”"
            />
          </form>
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
            {results.length} {results.length === 1 ? "article" : "articles"}
          </p>
          <p className="help-retrieval-mode" aria-label="Help retrieval mode">
            {retrievalMode === "provider-semantic"
              ? "Provider semantic ranking · corpus IDs preserved"
              : "Offline lexical index · citations preserved"}
          </p>
          {onSearchSemantic && semantic.status === "idle" && (
            <button
              type="button"
              className="help-semantic-search"
              onClick={beginSemanticRequest}
              disabled={!query.trim()}
            >
              Prepare meaning search
            </button>
          )}
          {onSearchSemantic && semantic.status === "confirm" && (
            <div className="help-semantic-confirm" role="alertdialog" aria-label="Confirm meaning search">
              <p>
                Send this query and article metadata to {assistantProviderLabel ?? "the selected provider"} for meaning-based ranking? No article body or workspace data will be sent.
              </p>
              <button type="button" className="primary" onClick={() => void confirmSemanticRequest()}>
                Search by meaning
              </button>
              <button type="button" onClick={() => setSemantic({ status: "idle" })}>
                Cancel
              </button>
            </div>
          )}
          {semantic.status === "loading" && <p className="help-retrieval-status" role="status">Ranking help by meaning…</p>}
          {semantic.status === "error" && <p className="help-retrieval-status" role="alert">{semantic.message}</p>}
          <ul className="help-list">
            {results.map(({ article, matchedTerms, confidence }) => (
              <li key={article.id}>
                <button
                  type="button"
                  className={`help-list-item ${article.id === selected?.id ? "is-selected" : ""}`}
                  aria-current={article.id === selected?.id ? "page" : undefined}
                  onClick={() => setSelectedId(article.id)}
                >
                  <span className="help-list-topic">{article.topic.replace("-", " ")}</span>
                  <strong>{article.title}</strong>
                  <span>{article.summary}</span>
                  {matchedTerms.length > 0 && (
                    <small>
                      Matches: {matchedTerms.slice(0, 3).join(", ")} · heuristic {Math.round(confidence * 100)}%
                    </small>
                  )}
                </button>
              </li>
            ))}
          </ul>
        </aside>

        <article className="help-article" aria-live="polite">
          {selected ? (
            <>
              <span className="help-article-topic">{selected.topic.replace("-", " ")}</span>
              <h3>{selected.title}</h3>
              <p className="help-article-summary">{selected.summary}</p>
              <p className="help-article-body">{selected.body}</p>
              <div className="help-source-card">
                <strong>Source-backed offline guidance</strong>
                <span>
                  Product corpus v1 · {selected.id} · retrieval: {retrievalMode === "provider-semantic" ? "provider semantic" : "offline lexical"} · exact-match priority
                </span>
                {query.trim() && selectedResult && (
                  <span>
                    {retrievalMode === "provider-semantic" ? "Provider ranking score" : "Heuristic match confidence"}: {Math.round(selectedResult.confidence * 100)}%
                    <span className="help-confidence-note"> · ranking signal only, not certification</span>
                  </span>
                )}
                <ul aria-label="Article sources">
                  {selected.sources.map((source) => (
                    <li key={source.id}>
                      <code>{source.path}</code> · {source.heading}
                    </li>
                  ))}
                </ul>
              </div>
              <p className="help-index-note">
                {retrievalMode === "provider-semantic"
                  ? `This ranking came from the configured provider and was constrained to the versioned article metadata. ${semantic.status === "results" ? semantic.uncertainty : "Uncertainty was not supplied."} The cited offline article remains the authority.`
                  : "This answer came from the deterministic offline index; no model or network was used. A future semantic retriever must preserve these article IDs, retrieval labels, and citation boundaries."}
              </p>
              <p className="help-assistant-note" role="status">
                {onAskAssistant
                  ? "Optional assistant: provider response is always confirmation-gated and citation-validated."
                  : "Optional assistant: not connected in this build. Cited offline guidance remains available without a provider."}
              </p>
              {onAskAssistant && (
                <section className="help-assistant-card" aria-label="Optional help assistant">
                  <strong>Ask the optional assistant</strong>
                  <p>
                    Only this article’s cited context will be offered to the
                    configured provider. No workspace, session, credential, or
                    clipboard data is included.
                  </p>
                  {assistant.status === "idle" && (
                    <button type="button" onClick={beginAssistantRequest}>
                      Prepare cited question
                    </button>
                  )}
                  {assistant.status === "confirm" && (
                    <div className="help-assistant-confirm" role="alertdialog" aria-label="Confirm assistant request">
                      <p>
                        Ready to send the cited article bundle ({assistant.request.sources.length} source{assistant.request.sources.length === 1 ? "" : "s"}) via {assistantProviderLabel ?? "the selected provider"}?
                      </p>
                      <button type="button" className="primary" onClick={() => void confirmAssistantRequest()}>
                        Send cited context
                      </button>
                      <button type="button" onClick={() => setAssistant({ status: "idle" })}>
                        Cancel
                      </button>
                    </div>
                  )}
                  {assistant.status === "loading" && <p role="status">Waiting for the configured provider…</p>}
                  {assistant.status === "answer" && (
                    <div className="help-assistant-answer" role="status">
                      <strong>Draft answer — not product truth until reviewed</strong>
                      <p>{assistant.answer.text}</p>
                      <small>Citations: {assistant.answer.citations.join(", ")}</small>
                      <small>Uncertainty: {assistant.answer.uncertainty}</small>
                    </div>
                  )}
                  {assistant.status === "error" && <p role="alert">{assistant.message}</p>}
                </section>
              )}
            </>
          ) : (
            <div className="help-empty" role="status">
              <h3>No matching guidance</h3>
              <p>Try a broader phrase, or clear the topic filter.</p>
            </div>
          )}
        </article>
      </div>
    </div>
  );
}
