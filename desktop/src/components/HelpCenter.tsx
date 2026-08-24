import { useEffect, useLayoutEffect, useMemo, useRef, useState, type KeyboardEvent as ReactKeyboardEvent } from "react";
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

type ConfirmKind = "semantic" | "assistant";

const FOCUSABLE_SELECTOR =
  'button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), a[href], [tabindex]:not([tabindex="-1"])';

function focusableIn(root: HTMLElement | null): HTMLElement[] {
  if (!root) return [];
  return Array.from(root.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR)).filter(
    (element) => !element.closest("[inert]"),
  );
}

function consentLayerPresent(): boolean {
  return Boolean(document.querySelector('[data-modal-layer="consent"]'));
}

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
  const [confirmStack, setConfirmStack] = useState<ConfirmKind[]>([]);
  const [consentPresent, setConsentPresent] = useState(false);
  const dialogRef = useRef<HTMLDivElement>(null);
  const surfaceRef = useRef<HTMLDivElement>(null);
  const returnFocusRef = useRef<HTMLElement | null>(null);
  const confirmDialogRef = useRef<HTMLDivElement>(null);
  const articleButtonRefs = useRef<Record<string, HTMLButtonElement | null>>({});
  const onCloseRef = useRef(onClose);
  const topConfirmRef = useRef<ConfirmKind | null>(null);
  const prevTopConfirmRef = useRef<ConfirmKind | null>(null);
  const layerReturnFocusRef = useRef<Partial<Record<ConfirmKind, HTMLElement | null>>>({});

  onCloseRef.current = onClose;

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

  const topConfirm = (() => {
    for (let index = confirmStack.length - 1; index >= 0; index -= 1) {
      const kind = confirmStack[index];
      if (kind === "assistant" && assistant.status === "confirm") return "assistant";
      if (kind === "semantic" && semantic.status === "confirm") return "semantic";
    }
    return null;
  })();
  topConfirmRef.current = topConfirm;

  const dismissConfirm = (kind: ConfirmKind) => {
    if (kind === "assistant") setAssistant({ status: "idle" });
    else setSemantic({ status: "idle" });
    setConfirmStack((stack) => stack.filter((entry) => entry !== kind));
  };

  const pushConfirm = (kind: ConfirmKind, opener: HTMLElement | null) => {
    layerReturnFocusRef.current[kind] = opener;
    setConfirmStack((stack) => [...stack.filter((entry) => entry !== kind), kind]);
  };

  useEffect(() => {
    if (!open) return;
    returnFocusRef.current = document.activeElement instanceof HTMLElement
      ? document.activeElement
      : null;
    prevTopConfirmRef.current = null;
    layerReturnFocusRef.current = {};
    const focusTarget = dialogRef.current?.querySelector<HTMLElement>("#help-search-input");
    if (!consentLayerPresent()) {
      focusTarget?.focus();
    }
    const onKey = (event: KeyboardEvent) => {
      if (consentLayerPresent()) return;
      if (event.key === "Escape") {
        event.preventDefault();
        const top = topConfirmRef.current;
        if (top === "assistant") {
          setAssistant({ status: "idle" });
          setConfirmStack((stack) => stack.filter((kind) => kind !== "assistant"));
        } else if (top === "semantic") {
          setSemantic({ status: "idle" });
          setConfirmStack((stack) => stack.filter((kind) => kind !== "semantic"));
        } else {
          onCloseRef.current();
        }
        return;
      }
      if (event.key !== "Tab") return;

      const trapRoot = topConfirmRef.current ? confirmDialogRef.current : dialogRef.current;
      const focusable = focusableIn(trapRoot);
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

  useLayoutEffect(() => {
    if (!open) {
      prevTopConfirmRef.current = null;
      return;
    }
    if (consentLayerPresent()) return;
    const previous = prevTopConfirmRef.current;
    prevTopConfirmRef.current = topConfirm;
    if (topConfirm) {
      const focusTarget = confirmDialogRef.current?.querySelector<HTMLElement>(
        "button.primary, button:not([disabled])",
      );
      focusTarget?.focus();
      return;
    }
    if (previous) {
      layerReturnFocusRef.current[previous]?.focus();
    }
  }, [open, topConfirm, consentPresent]);

  const helpFocusBeforeConsentRef = useRef<HTMLElement | null>(null);
  const prevConsentPresentRef = useRef(false);
  useLayoutEffect(() => {
    if (!open) {
      helpFocusBeforeConsentRef.current = null;
      prevConsentPresentRef.current = false;
      return;
    }
    if (consentPresent) {
      prevConsentPresentRef.current = true;
      if (
        dialogRef.current?.contains(document.activeElement) &&
        document.activeElement instanceof HTMLElement
      ) {
        helpFocusBeforeConsentRef.current = document.activeElement;
      }
      return;
    }
    if (!prevConsentPresentRef.current) return;
    prevConsentPresentRef.current = false;
    helpFocusBeforeConsentRef.current?.focus();
    helpFocusBeforeConsentRef.current = null;
  }, [open, consentPresent]);

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

  useEffect(() => {
    if (!open) {
      setConsentPresent(false);
      return;
    }
    const read = () => setConsentPresent(consentLayerPresent());
    read();
    const observer = new MutationObserver(read);
    observer.observe(document.body, {
      childList: true,
      subtree: true,
      attributes: true,
      attributeFilter: ["data-modal-layer"],
    });
    return () => observer.disconnect();
  }, [open]);

  useEffect(() => {
    if (selected && !results.some((result) => result.article.id === selectedId)) {
      setSelectedId(selected.id);
    }
  }, [results, selected, selectedId]);

  useEffect(() => {
    setAssistant({ status: "idle" });
    setConfirmStack((stack) => stack.filter((kind) => kind !== "assistant"));
  }, [selectedId, query]);

  useEffect(() => {
    setSemantic({ status: "idle" });
    setConfirmStack((stack) => stack.filter((kind) => kind !== "semantic"));
  }, [query, topic]);

  const beginAssistantRequest = (opener: HTMLElement | null = null) => {
    if (!selected || !onAskAssistant) return;
    pushConfirm(
      "assistant",
      opener ?? (document.activeElement instanceof HTMLElement ? document.activeElement : null),
    );
    setAssistant({
      status: "confirm",
      request: buildHelpAssistantRequest(selected, query || selected.title, retrievalMode),
    });
  };

  const confirmAssistantRequest = async () => {
    if (assistant.status !== "confirm" || !onAskAssistant) return;
    setConfirmStack((stack) => stack.filter((kind) => kind !== "assistant"));
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

  const beginSemanticRequest = (opener: HTMLElement | null = null) => {
    const trimmed = query.trim();
    if (!trimmed || !onSearchSemantic) return;
    pushConfirm(
      "semantic",
      opener ?? (document.activeElement instanceof HTMLElement ? document.activeElement : null),
    );
    setSemantic({
      status: "confirm",
      request: buildHelpSemanticRequest(
        trimmed,
        HELP_ARTICLES.filter((article) => topic === "all" || article.topic === topic),
      ),
    });
  };

  const confirmSemanticRequest = async () => {
    if (semantic.status !== "confirm" || !onSearchSemantic) return;
    setConfirmStack((stack) => stack.filter((kind) => kind !== "semantic"));
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

  const moveArticleSelection = (
    event: ReactKeyboardEvent<HTMLButtonElement>,
    index: number,
  ) => {
    if (results.length === 0) return;
    let nextIndex: number | null = null;
    if (event.key === "ArrowDown") nextIndex = (index + 1) % results.length;
    if (event.key === "ArrowUp") nextIndex = (index - 1 + results.length) % results.length;
    if (event.key === "Home") nextIndex = 0;
    if (event.key === "End") nextIndex = results.length - 1;
    if (nextIndex === null) return;

    event.preventDefault();
    const nextId = results[nextIndex].article.id;
    setSelectedId(nextId);
    articleButtonRefs.current[nextId]?.focus();
  };

  if (!open) return null;

  return (
    <div
      ref={dialogRef}
      className="help-center"
      data-modal-layer="help"
      role="dialog"
      aria-modal={consentPresent ? false : true}
      aria-hidden={consentPresent ? true : undefined}
      aria-labelledby="help-center-title"
      aria-describedby="help-center-subtitle"
      {...(consentPresent ? { inert: "" } : {})}
    >
      <div
        className="help-surface"
        ref={surfaceRef}
        aria-hidden={topConfirm ? true : undefined}
        {...(topConfirm ? { inert: "" } : {})}
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

          <p className="help-result-count">
            {results.length} {results.length === 1 ? "article" : "articles"}
          </p>
          <p className="help-retrieval-mode" aria-label="Help retrieval mode">
            {retrievalMode === "provider-semantic"
              ? "Provider semantic ranking · corpus IDs preserved"
              : "Offline lexical index · citations preserved"}
          </p>
            {onSearchSemantic && (semantic.status === "idle" || semantic.status === "confirm") && (
            <button
              type="button"
              className="help-semantic-search"
              onClick={(event) => beginSemanticRequest(event.currentTarget)}
              disabled={!query.trim()}
            >
              Prepare meaning search
            </button>
          )}
          {semantic.status === "loading" && <p className="help-retrieval-status" role="status">Ranking help by meaning…</p>}
          {semantic.status === "error" && <p className="help-retrieval-status" role="alert">{semantic.message}</p>}
          <ul className="help-list" role="listbox" aria-label="Help article results">
            {results.map(({ article, matchedTerms, confidence }, index) => (
              <li key={article.id}>
                <button
                  type="button"
                  role="option"
                  id={`help-article-option-${article.id}`}
                  ref={(element) => {
                    articleButtonRefs.current[article.id] = element;
                  }}
                  tabIndex={article.id === selected?.id ? 0 : -1}
                  className={`help-list-item ${article.id === selected?.id ? "is-selected" : ""}`}
                  aria-selected={article.id === selected?.id}
                  onClick={() => setSelectedId(article.id)}
                  onKeyDown={(event) => moveArticleSelection(event, index)}
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

        <p className="sr-only" role="status" aria-live="polite">
          {selected ? `Selected article: ${selected.title}` : "No matching guidance"}
        </p>
        <article className="help-article">
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
                  {(assistant.status === "idle" || assistant.status === "confirm") && (
                    <button type="button" onClick={(event) => beginAssistantRequest(event.currentTarget)}>
                      Prepare cited question
                    </button>
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

      {topConfirm === "semantic" && semantic.status === "confirm" && (
        <div className="help-confirm-layer">
          <div
            ref={confirmDialogRef}
            className="help-semantic-confirm"
            role="alertdialog"
            aria-modal="true"
            aria-labelledby="help-semantic-confirm-title"
            aria-describedby="help-semantic-confirm-copy"
          >
            <p>
              <strong id="help-semantic-confirm-title">Confirm meaning search</strong>
            </p>
            <p id="help-semantic-confirm-copy">
              Send this query and article metadata to {assistantProviderLabel ?? "the selected provider"} for meaning-based ranking? No article body or workspace data will be sent.
            </p>
            <details className="help-confirm-details" open>
              <summary>Review exact metadata</summary>
              <p>Query: <code>{semantic.request.query}</code></p>
              <ul aria-label="Meaning search metadata">
                {semantic.request.candidates.map((candidate) => (
                  <li key={candidate.articleId}>
                    <code>{candidate.articleId}</code> · {candidate.sources.map((source) => `${source.id} · ${source.path}`).join(", ")}
                  </li>
                ))}
              </ul>
            </details>
            <button type="button" className="primary" onClick={() => void confirmSemanticRequest()}>
              Search by meaning
            </button>
            <button type="button" onClick={() => dismissConfirm("semantic")}>
              Cancel
            </button>
          </div>
        </div>
      )}
      {topConfirm === "assistant" && assistant.status === "confirm" && (
        <div className="help-confirm-layer">
          <div
            ref={confirmDialogRef}
            className="help-assistant-confirm"
            role="alertdialog"
            aria-modal="true"
            aria-labelledby="help-assistant-confirm-title"
            aria-describedby="help-assistant-confirm-copy"
          >
            <p>
              <strong id="help-assistant-confirm-title">Confirm assistant request</strong>
            </p>
            <p id="help-assistant-confirm-copy">
              Ready to send the cited article bundle ({assistant.request.sources.length} source{assistant.request.sources.length === 1 ? "" : "s"}) via {assistantProviderLabel ?? "the selected provider"}?
            </p>
            <details className="help-confirm-details" open>
              <summary>Review exact cited sources</summary>
              <ul aria-label="Assistant request sources">
                {assistant.request.sources.map((source) => (
                  <li key={source.id}>
                    <code>{source.id}</code> · {source.path} · {source.heading}
                  </li>
                ))}
              </ul>
            </details>
            <button type="button" className="primary" onClick={() => void confirmAssistantRequest()}>
              Send cited context
            </button>
            <button type="button" onClick={() => dismissConfirm("assistant")}>
              Cancel
            </button>
          </div>
        </div>
      )}
    </div>
  );
}
