/**
 * Desktop Help Center: the reference consumer of the canonical Help authority.
 *
 * Retrieval, abstention, citation, and reply validation all belong to
 * `helpAuthority.ts` / `helpAnswer.ts`; the presentation rules belong to
 * `helpCenterView.ts`. This file owns what is left, and only what is left:
 * focus, keyboard, live regions, confirmation gates, and the one thing a
 * headless contract cannot do for a UI — actually enforcing the timeout it
 * declared.
 *
 * Three properties are load-bearing here and are covered by tests:
 *
 *   - **An abstention never renders as an answer.** The view state exposes
 *     `answer` for exactly one status; every other status renders candidates
 *     under a banner that says what they are.
 *   - **Nothing leaves the machine without a confirmation.** Retrieval is
 *     offline and synchronous, so there is no spinner and no network for
 *     search. The optional model seams are the only outbound paths, and each
 *     is gated by its own dialog showing exactly what would be sent.
 *   - **Unknowns stay unknown.** Provider, model, cost, and latency are
 *     rendered from the request's own `unknowns`. A timeout is reported
 *     against the declared budget, never as a measured duration.
 *
 * The legacy props (`onAskAssistant`, `onSearchSemantic`,
 * `assistantProviderLabel`) keep working against the legacy `helpCenter.ts`
 * builders and validators, so an existing embedder is unchanged. The legacy
 * ranking seam is the one behaviour deliberately narrowed: a provider may
 * reorder candidates, and may no longer turn an abstention into an answer.
 */

import {
  useCallback,
  useEffect,
  useId,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import {
  buildHelpAnswerRequest,
  parseHelpAnswerResponse,
  validateHelpAnswerResponse,
  type HelpAnswerRefusal,
  type HelpAnswerRequest,
  type HelpAnswerResponse,
} from "../lib/helpAnswer";
import {
  createHelpAuthority,
  helpArticleText,
  type HelpAuthority,
  type HelpAuthorityAudience,
  type HelpHit,
} from "../lib/helpAuthority";
import {
  buildHelpAssistantRequest,
  buildHelpSemanticRequest,
  validateHelpAssistantAnswer,
  validateHelpSemanticAnswer,
  type HelpArticle,
  type HelpAssistantAnswer,
  type HelpAssistantRequest,
  type HelpSemanticAnswer,
  type HelpSemanticRequest,
  type HelpTopic,
} from "../lib/helpCenter";
import {
  describeHelpAskTimeout,
  describeHelpAskUnknowns,
  helpArticleView,
  helpBrowseArticles,
  helpViewState,
  summarizeHelpAnswer,
  type HelpAskSummary,
  type HelpViewArticle,
  type HelpViewCandidate,
} from "../lib/helpCenterView";

export type HelpCenterProps = {
  open: boolean;
  onClose: () => void;
  /** Optional provider adapter; the UI still requires confirmation before it is called. */
  onAskAssistant?: (request: HelpAssistantRequest) => Promise<HelpAssistantAnswer>;
  /** Optional meaning-based ranking adapter; the UI requires confirmation before it is called. */
  onSearchSemantic?: (request: HelpSemanticRequest) => Promise<HelpSemanticAnswer>;
  assistantProviderLabel?: string;
  /**
   * Cited answer seam over the canonical corpus. Receives the bounded request
   * and an abort signal, and returns the provider's raw reply text — parsing
   * and validation stay here so an adapter cannot widen what is accepted.
   */
  onAnswer?: (request: HelpAnswerRequest, signal: AbortSignal) => Promise<string>;
  /** Corpus to serve. Defaults to the shipped, digest-verified authority. */
  authority?: HelpAuthority;
  /** The viewer's audience, as the embedder declares it. Filters; grants nothing. */
  audience?: HelpAuthorityAudience;
  /** Include gated and operator articles. The embedder's declaration, not a grant. */
  includeRestricted?: boolean;
  /** Budget for the optional answer seam, inside the contract's own bounds. */
  answerTimeoutMs?: number;
};

const TOPICS: Array<{ value: HelpTopic | "all"; label: string }> = [
  { value: "all", label: "All topics" },
  { value: "getting-started", label: "Getting started" },
  { value: "providers", label: "Providers" },
  { value: "computer-use", label: "Computer Use" },
  { value: "operations", label: "Operations" },
];

/** The shipped authority is built once: it validates and digests its corpus. */
let sharedAuthority: HelpAuthority | null = null;
function shippedAuthority(): HelpAuthority {
  if (sharedAuthority === null) sharedAuthority = createHelpAuthority();
  return sharedAuthority;
}

type AskState =
  | { status: "idle" }
  | { status: "refused"; refusal: HelpAnswerRefusal }
  | { status: "confirm"; request: HelpAnswerRequest }
  | { status: "pending"; request: HelpAnswerRequest }
  | {
      status: "settled";
      request: HelpAnswerRequest;
      summary: HelpAskSummary;
      response: HelpAnswerResponse | null;
    };

type AssistantState =
  | { status: "idle" }
  | { status: "confirm"; request: HelpAssistantRequest }
  | { status: "loading" }
  | { status: "answer"; answer: HelpAssistantAnswer }
  | { status: "error"; message: string };

type RerankState =
  | { status: "idle" }
  | { status: "confirm"; request: HelpSemanticRequest }
  | { status: "loading" }
  | { status: "ranked"; order: readonly string[]; uncertainty: string }
  | { status: "error"; message: string };

type ConfirmKind = "semantic" | "assistant" | "answer";

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
function rankSignal(confidence: number): string {
  return `${Math.round(confidence * 100)}%`;
}

/**
 * Adapt a canonical article to the legacy article shape.
 *
 * The legacy seam is kept byte-for-byte compatible by feeding the legacy
 * builder rather than reimplementing it. A canonical article's prose is its
 * passages, which is exactly what `helpArticleText` joins, so the adaptation
 * loses nothing an assistant request carried before.
 */
function legacyArticleFor(authority: HelpAuthority, articleId: string): HelpArticle | null {
  const article = authority.article(articleId);
  if (!article) return null;
  return {
    id: article.id,
    title: article.title,
    topic: article.topic,
    summary: article.summary,
    body: helpArticleText(article),
    aliases: article.aliases,
    keywords: article.keywords,
    sources: article.sources.map((source) => ({ ...source })),
  };
}

export function HelpCenter({
  open,
  onClose,
  onAskAssistant,
  onSearchSemantic,
  assistantProviderLabel,
  onAnswer,
  authority: authorityProp,
  audience,
  includeRestricted = false,
  answerTimeoutMs,
}: HelpCenterProps) {
  const authority = authorityProp ?? shippedAuthority();
  const [query, setQuery] = useState("");
  const [topic, setTopic] = useState<HelpTopic | "all">("all");
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [activeIndex, setActiveIndex] = useState(0);
  const [ask, setAsk] = useState<AskState>({ status: "idle" });
  const [assistant, setAssistant] = useState<AssistantState>({ status: "idle" });
  const [rerank, setRerank] = useState<RerankState>({ status: "idle" });
  const [confirmStack, setConfirmStack] = useState<ConfirmKind[]>([]);

  const dialogRef = useRef<HTMLDivElement>(null);
  const listRef = useRef<HTMLUListElement>(null);
  const returnFocusRef = useRef<HTMLElement | null>(null);
  const confirmDialogRef = useRef<HTMLDivElement>(null);
  const onCloseRef = useRef(onClose);
  const topConfirmRef = useRef<ConfirmKind | null>(null);
  const prevTopConfirmRef = useRef<ConfirmKind | null>(null);
  const layerReturnFocusRef = useRef<Partial<Record<ConfirmKind, HTMLElement | null>>>({});
  const askAbortRef = useRef<AbortController | null>(null);
  const askTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  onCloseRef.current = onClose;

  const listboxId = useId();
  const optionId = (articleId: string) => `${listboxId}-${articleId.replace(/[^a-zA-Z0-9-]/g, "_")}`;

  /* ---------------- retrieval: pure, offline, synchronous ---------------- */

  const result = useMemo(
    () => authority.search(query, { topic, audience, includeRestricted }),
    [authority, query, topic, audience, includeRestricted],
  );
  const view = useMemo(() => helpViewState(result, authority), [result, authority]);

  const browseList = useMemo(
    () => (view.status === "browse"
      ? helpBrowseArticles(authority.articles, { topic, audience, includeRestricted })
      : null),
    [view.status, authority, topic, audience, includeRestricted],
  );

  /**
   * What the reader can pick from.
   *
   * In `answer` the leader heads the list and its candidates follow; in an
   * abstention there is no leader, only candidates; in `browse` it is the
   * filtered corpus. A rejected query lists nothing: it was never searched.
   */
  const listed: readonly HelpViewCandidate[] = useMemo(() => {
    if (browseList) return browseList;
    if (view.answer) {
      const leader: HelpViewCandidate = {
        articleId: view.answer.articleId,
        title: view.answer.title,
        summary: view.answer.summary,
        topic: view.answer.topic,
        confidence: view.answer.confidence,
        matchedTerms: view.answer.matchedTerms,
        labels: view.answer.labels,
      };
      return [leader, ...view.candidates];
    }
    return view.candidates;
  }, [browseList, view]);

  /**
   * Apply an optional provider re-ranking to the candidate order.
   *
   * The provider may reorder what retrieval already found. It may not add an
   * article, and — unlike the seam this replaces — it may not promote an
   * abstention into an answer: `view.answer` is untouched by anything here.
   */
  const ordered: readonly HelpViewCandidate[] = useMemo(() => {
    if (rerank.status !== "ranked") return listed;
    const rank = new Map(rerank.order.map((id, index) => [id, index]));
    return [...listed].sort((a, b) =>
      (rank.get(a.articleId) ?? Number.MAX_SAFE_INTEGER) -
      (rank.get(b.articleId) ?? Number.MAX_SAFE_INTEGER));
  }, [listed, rerank]);

  const selected = useMemo(
    () => ordered.find((candidate) => candidate.articleId === selectedId) ?? ordered[0] ?? null,
    [ordered, selectedId],
  );

  /**
   * The detail for the selected article, and whether it is *the* answer.
   *
   * Selecting a candidate while retrieval answered does not make that
   * candidate the answer; the banner and the article card both say so.
   */
  const detail: HelpViewArticle | null = useMemo(() => {
    if (!selected) return null;
    if (view.answer && view.answer.articleId === selected.articleId) return view.answer;
    const hit: HelpHit | undefined = result.hits.find(
      (candidate) => candidate.article.id === selected.articleId,
    );
    if (hit) return helpArticleView(hit, authority);
    const article = authority.article(selected.articleId);
    if (!article) return null;
    // Browsing: there is no query, so there is no ranking and no citation
    // span. The article's own sources are still shown.
    return helpArticleView(
      {
        article,
        score: 0,
        confidence: 0,
        matchedTerms: [],
        explanation: {
          tokenScore: 0, lexicalScore: 0, score: 0, confidence: 0, coverage: 0, signals: [],
        },
        citation: { articleId: article.id, sources: article.sources, spans: [] },
      },
      authority,
    );
  }, [selected, view.answer, result.hits, authority]);

  const isPresentedAnswer =
    view.status === "answer" && detail !== null && detail.articleId === view.answer?.articleId;

  /* ---------------------------- confirmations ---------------------------- */

  const topConfirm = (() => {
    for (let index = confirmStack.length - 1; index >= 0; index -= 1) {
      const kind = confirmStack[index];
      if (kind === "assistant" && assistant.status === "confirm") return "assistant";
      if (kind === "semantic" && rerank.status === "confirm") return "semantic";
      if (kind === "answer" && ask.status === "confirm") return "answer";
    }
    return null;
  })();
  topConfirmRef.current = topConfirm;

  const dropConfirm = (kind: ConfirmKind) =>
    setConfirmStack((stack) => stack.filter((entry) => entry !== kind));

  const dismissConfirm = (kind: ConfirmKind) => {
    if (kind === "assistant") setAssistant({ status: "idle" });
    else if (kind === "semantic") setRerank({ status: "idle" });
    else setAsk({ status: "idle" });
    dropConfirm(kind);
  };

  const pushConfirm = (kind: ConfirmKind, opener: HTMLElement | null) => {
    layerReturnFocusRef.current[kind] = opener;
    setConfirmStack((stack) => [...stack.filter((entry) => entry !== kind), kind]);
  };

  const openerOr = (opener: HTMLElement | null) =>
    opener ?? (document.activeElement instanceof HTMLElement ? document.activeElement : null);

  /* ------------------------- focus and keyboard -------------------------- */

  useEffect(() => {
    if (!open) return;
    returnFocusRef.current = document.activeElement instanceof HTMLElement
      ? document.activeElement
      : null;
    prevTopConfirmRef.current = null;
    layerReturnFocusRef.current = {};
    dialogRef.current?.querySelector<HTMLElement>("#help-search-input")?.focus();

    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        const top = topConfirmRef.current;
        if (top === null) onCloseRef.current();
        else dismissConfirm(top);
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
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  useLayoutEffect(() => {
    if (!open) {
      prevTopConfirmRef.current = null;
      return;
    }
    const previous = prevTopConfirmRef.current;
    prevTopConfirmRef.current = topConfirm;
    if (topConfirm) {
      confirmDialogRef.current
        ?.querySelector<HTMLElement>("button.primary, button:not([disabled])")
        ?.focus();
      return;
    }
    if (previous) layerReturnFocusRef.current[previous]?.focus();
  }, [open, topConfirm]);

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

  /** Keep the active option inside the list a query or filter just rebuilt. */
  useEffect(() => {
    setActiveIndex((index) => (index < ordered.length ? index : 0));
  }, [ordered.length]);

  const commitActive = useCallback(
    (index: number) => {
      const candidate = ordered[index];
      if (!candidate) return;
      setActiveIndex(index);
      setSelectedId(candidate.articleId);
    },
    [ordered],
  );

  /**
   * Combobox keys, per the listbox pattern.
   *
   * Options are not tab stops: the input keeps focus and names the active
   * option through `aria-activedescendant`, so a screen-reader user hears each
   * result without leaving the field they are typing in. Escape is not bound
   * here — it belongs to the dialog, and stealing it would strand a keyboard
   * user inside the search box.
   */
  const onSearchKeyDown = (event: React.KeyboardEvent<HTMLInputElement>) => {
    if (ordered.length === 0) return;
    const last = ordered.length - 1;
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
    const active = ordered[activeIndex];
    if (!active || !listRef.current) return;
    const option = listRef.current.querySelector<HTMLElement>(
      `[data-article-id="${CSS.escape(active.articleId)}"]`,
    );
    // Keeping the active option visible is a convenience, not a guarantee:
    // scrollIntoView is absent in some hosts, and a missing scroll must never
    // break the keyboard navigation it was meant to assist.
    if (option && typeof option.scrollIntoView === "function") {
      option.scrollIntoView({ block: "nearest" });
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeIndex, open, ordered]);

  /* ----------------------- seam lifecycle resets ------------------------- */

  const clearAskTimer = () => {
    if (askTimerRef.current !== null) {
      clearTimeout(askTimerRef.current);
      askTimerRef.current = null;
    }
  };

  const abortAsk = useCallback(() => {
    clearAskTimer();
    askAbortRef.current?.abort();
    askAbortRef.current = null;
  }, []);

  useEffect(() => abortAsk, [abortAsk]);

  // A new question, article, or corpus filter invalidates every in-flight or
  // completed provider exchange: an answer about one article must never stay
  // on screen next to another.
  useEffect(() => {
    abortAsk();
    setAsk({ status: "idle" });
    setAssistant({ status: "idle" });
    setConfirmStack((stack) => stack.filter((kind) => kind === "semantic"));
  }, [selected?.articleId, query, abortAsk]);

  useEffect(() => {
    setRerank({ status: "idle" });
    setConfirmStack((stack) => stack.filter((kind) => kind !== "semantic"));
  }, [query, topic, audience, includeRestricted]);

  /* --------------------- optional cited answer seam ---------------------- */

  const beginAnswerRequest = (opener: HTMLElement | null = null) => {
    if (!onAnswer || !view.canAskModel) return;
    const built = buildHelpAnswerRequest(
      result,
      answerTimeoutMs === undefined ? {} : { timeoutMs: answerTimeoutMs },
    );
    if (!built.ok) {
      // The seam refused. Show its reason: a button that silently does nothing
      // reads as a broken app, and the refusal is the honest answer.
      setAsk({ status: "refused", refusal: built.refusal });
      return;
    }
    pushConfirm("answer", openerOr(opener));
    setAsk({ status: "confirm", request: built.request });
  };

  const confirmAnswerRequest = async () => {
    if (ask.status !== "confirm" || !onAnswer) return;
    const request = ask.request;
    dropConfirm("answer");
    setAsk({ status: "pending", request });

    const controller = new AbortController();
    askAbortRef.current = controller;
    let timedOut = false;
    const timeout = new Promise<never>((_, reject) => {
      askTimerRef.current = setTimeout(() => {
        timedOut = true;
        controller.abort();
        reject(new Error("help-answer-timeout"));
      }, request.timeoutMs);
    });

    try {
      const reply = await Promise.race([onAnswer(request, controller.signal), timeout]);
      clearAskTimer();
      const response = parseHelpAnswerResponse(reply);
      const validation = validateHelpAnswerResponse(response, request);
      setAsk({
        status: "settled",
        request,
        summary: summarizeHelpAnswer(response, validation),
        response: validation.accepted ? response : null,
      });
    } catch {
      clearAskTimer();
      setAsk({
        status: "settled",
        request,
        summary: timedOut
          ? describeHelpAskTimeout(request)
          : {
              status: "failed",
              headline: "No reply",
              detail:
                "The request did not complete. Whether it reached a provider is unknown. " +
                "The cited documentation is unchanged.",
              corpusRemainsAuthority: true,
            },
        response: null,
      });
    } finally {
      askAbortRef.current = null;
    }
  };

  /* ------------------------ legacy assistant seam ------------------------ */

  const beginAssistantRequest = (opener: HTMLElement | null = null) => {
    if (!selected || !onAskAssistant) return;
    const legacy = legacyArticleFor(authority, selected.articleId);
    if (!legacy) return;
    pushConfirm("assistant", openerOr(opener));
    setAssistant({
      status: "confirm",
      request: buildHelpAssistantRequest(legacy, query || legacy.title, "offline-lexical"),
    });
  };

  const confirmAssistantRequest = async () => {
    if (assistant.status !== "confirm" || !onAskAssistant) return;
    const request = assistant.request;
    dropConfirm("assistant");
    setAssistant({ status: "loading" });
    try {
      const answer = await onAskAssistant(request);
      const validation = validateHelpAssistantAnswer(
        answer,
        request.sources.map((source) => source.id),
      );
      if (!validation.accepted) {
        setAssistant({
          status: "error",
          message: `Assistant answer rejected: ${validation.reason}.`,
        });
        return;
      }
      setAssistant({ status: "answer", answer });
    } catch {
      setAssistant({
        status: "error",
        message: "Assistant unavailable; cited offline guidance remains authoritative.",
      });
    }
  };

  /* ------------------- legacy provider re-ranking seam ------------------- */

  const beginRerankRequest = (opener: HTMLElement | null = null) => {
    const trimmed = query.trim();
    if (!trimmed || !onSearchSemantic || listed.length === 0) return;
    const articles = listed
      .map((candidate) => legacyArticleFor(authority, candidate.articleId))
      .filter((article): article is HelpArticle => article !== null);
    if (articles.length === 0) return;
    pushConfirm("semantic", openerOr(opener));
    setRerank({ status: "confirm", request: buildHelpSemanticRequest(trimmed, articles) });
  };

  const confirmRerankRequest = async () => {
    if (rerank.status !== "confirm" || !onSearchSemantic) return;
    const request = rerank.request;
    dropConfirm("semantic");
    setRerank({ status: "loading" });
    try {
      const answer = await onSearchSemantic(request);
      const validation = validateHelpSemanticAnswer(
        answer,
        request.candidates.map((candidate) => candidate.articleId),
      );
      if (!validation.accepted) {
        setRerank({ status: "error", message: `Semantic ranking rejected: ${validation.reason}.` });
        return;
      }
      setRerank({
        status: "ranked",
        order: answer.results.slice().sort((a, b) => b.score - a.score)
          .map((entry) => entry.articleId),
        uncertainty: answer.uncertainty,
      });
    } catch {
      setRerank({
        status: "error",
        message: "Semantic ranking unavailable; offline retrieval remains available.",
      });
    }
  };

  if (!open) return null;

  const providerLabel = assistantProviderLabel ?? "the selected provider";
  const askUnknowns =
    ask.status === "idle" || ask.status === "refused"
      ? null
      : describeHelpAskUnknowns(ask.request, assistantProviderLabel);
  const listLabel = view.status === "browse" ? "Help articles" : "Help search results";

  return (
    <div
      ref={dialogRef}
      className="help-center"
      data-modal-layer="help"
      role="dialog"
      aria-modal="true"
      aria-labelledby="help-center-title"
      aria-describedby="help-center-subtitle"
    >
      <div
        className="help-surface"
        aria-hidden={topConfirm ? true : undefined}
        {...(topConfirm ? { inert: "" } : {})}
      >
        <header className="help-header">
          <div>
            <p className="help-eyebrow">GrokPtah guidance</p>
            <h2 id="help-center-title">Help Center</h2>
            <p className="help-subtitle" id="help-center-subtitle">
              Search the shipped documentation. Retrieval runs on this machine and
              cites what it returns.
            </p>
          </div>
          <button type="button" onClick={onClose} aria-label="Close Help Center">
            Close <span aria-hidden>Esc</span>
          </button>
        </header>

        <div className="help-layout">
          <aside className="help-nav" aria-label="Help search">
            <form
              className="help-search"
              role="search"
              onSubmit={(event) => event.preventDefault()}
            >
              <label htmlFor="help-search-input">Search help</label>
              <input
                id="help-search-input"
                role="combobox"
                aria-label="Search help"
                aria-expanded={ordered.length > 0}
                aria-controls={listboxId}
                aria-autocomplete="list"
                aria-describedby="help-search-hint"
                aria-activedescendant={
                  ordered[activeIndex] ? optionId(ordered[activeIndex].articleId) : undefined
                }
                autoComplete="off"
                value={query}
                onChange={(event) => setQuery(event.target.value)}
                onKeyDown={onSearchKeyDown}
                placeholder="Ask in your own words, or try “quota”, “stale frame”"
              />
            </form>
            <p className="sr-only" id="help-search-hint">
              Results update as you type. Use the up and down arrow keys to review
              them, Home and End to jump, and Enter to open the highlighted
              article. Escape closes the Help Center.
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
              {ordered.length} {ordered.length === 1 ? "article" : "articles"}
              {view.totalMatched > ordered.length && view.status !== "browse"
                ? ` shown of ${view.totalMatched} matched`
                : ""}
            </p>
            <p className="help-retrieval-mode" aria-label="Help retrieval mode">
              Offline hybrid retrieval · no network, no model · corpus{" "}
              <span className="help-digest">{view.corpusVersion}</span>
            </p>

            {onSearchSemantic && (rerank.status === "idle" || rerank.status === "confirm") && (
              <button
                type="button"
                className="help-semantic-search"
                onClick={(event) => beginRerankRequest(event.currentTarget)}
                disabled={!query.trim() || listed.length === 0}
              >
                Prepare meaning search
              </button>
            )}
            {rerank.status === "loading" && (
              <p className="help-retrieval-status" role="status">
                Waiting for the configured provider to re-order these results…
              </p>
            )}
            {rerank.status === "ranked" && (
              <p className="help-retrieval-status" role="status">
                Provider re-ordered these results. It added nothing and changed no
                outcome. {rerank.uncertainty}
              </p>
            )}
            {rerank.status === "error" && (
              <p className="help-retrieval-status" role="alert">
                {rerank.message}
              </p>
            )}

            <ul
              className="help-list"
              id={listboxId}
              ref={listRef}
              role="listbox"
              aria-label={listLabel}
            >
              {ordered.map((candidate, index) => (
                <li
                  key={candidate.articleId}
                  id={optionId(candidate.articleId)}
                  data-article-id={candidate.articleId}
                  role="option"
                  className={
                    "help-list-item" +
                    (candidate.articleId === selected?.articleId ? " is-selected" : "") +
                    (index === activeIndex ? " is-active" : "")
                  }
                  aria-selected={candidate.articleId === selected?.articleId}
                  onClick={() => commitActive(index)}
                >
                  <span className="help-list-topic">{topicLabel(candidate.topic)}</span>
                  <strong>{candidate.title}</strong>
                  <span>{candidate.summary}</span>
                  <span className="help-list-labels">
                    <span
                      className={`help-access help-access-${candidate.labels.access.value}`}
                    >
                      {candidate.labels.access.label}
                    </span>
                    {view.status === "browse" ? null
                      : candidate.articleId === view.answer?.articleId ? (
                        <span className="help-badge-answer">Answer</span>
                      ) : (
                        <span className="help-badge-suggestion">Suggestion</span>
                      )}
                  </span>
                  {candidate.matchedTerms.length > 0 && (
                    <small>
                      Matched {candidate.matchedTerms.slice(0, 3).join(", ")} · rank
                      signal {rankSignal(candidate.confidence)} — a ranking signal,
                      not a certification
                    </small>
                  )}
                </li>
              ))}
            </ul>
          </aside>

          <p className="sr-only" role="status" aria-live="polite">
            {view.headline}. {detail ? `Showing ${detail.title}.` : "Nothing is shown."}
          </p>

          <article className="help-article">
            <section
              className={`help-state help-state-${view.status}`}
              role={view.status === "rejected" ? "alert" : "status"}
              aria-label="Help retrieval outcome"
            >
              <strong className="help-state-headline">{view.headline}</strong>
              <p className="help-state-detail">{view.detail}</p>
              {view.status !== "browse" && view.status !== "rejected" && (
                <p className="help-state-verdict">
                  Retriever verdict: <code>{view.outcome}</code>
                  {view.abstainReason ? (
                    <>
                      {" "}
                      (<code>{view.abstainReason}</code>)
                    </>
                  ) : null}
                  {" · "}
                  {view.totalMatched} matched
                </p>
              )}
              {view.status === "rejected" && view.rejection && (
                <p className="help-state-verdict">
                  Rejected: <code>{view.rejection}</code>
                </p>
              )}
            </section>

            {detail ? (
              <>
                {!isPresentedAnswer && view.status !== "browse" && (
                  <p className="help-suggestion-note" role="note">
                    Shown as a suggestion. Help did not present this article as the
                    answer to “{view.query}”.
                  </p>
                )}
                <span className="help-article-topic">{topicLabel(detail.topic)}</span>
                <h3>{detail.title}</h3>
                <p className="help-article-summary">{detail.summary}</p>

                <section className="help-labels" aria-label="Article access and capabilities">
                  <p className="help-label-row">
                    <span
                      className={`help-access help-access-${detail.labels.access.value}`}
                    >
                      {detail.labels.access.label}
                    </span>
                    <span className="help-label-detail">{detail.labels.access.detail}</span>
                  </p>
                  <p className="help-label-row">
                    <span className="help-label-key">Written for</span>
                    <span>
                      {detail.labels.audience.map((entry) => entry.label).join(", ")}
                    </span>
                  </p>
                  {detail.labels.capabilities.length > 0 ? (
                    <>
                      <ul className="help-capabilities" aria-label="Documented capabilities">
                        {detail.labels.capabilities.map((capability) => (
                          <li key={capability.id} className="help-capability">
                            <span className="help-capability-label">{capability.label}</span>
                            <code>{capability.id}</code>
                            <span className="help-capability-live">
                              live: {capability.liveAvailability}
                            </span>
                          </li>
                        ))}
                      </ul>
                      <p className="help-capability-note">
                        {detail.labels.liveAvailabilityNote}
                      </p>
                    </>
                  ) : (
                    <p className="help-label-row">
                      <span className="help-label-key">Capabilities</span>
                      <span>This article documents none.</span>
                    </p>
                  )}
                </section>

                {detail.passages.map((passage) => (
                  <div className="help-passage" key={passage.id}>
                    <p className="help-article-body">{passage.text}</p>
                    <p className="help-passage-sources">
                      From{" "}
                      {passage.sources.map((source) => `${source.path} — ${source.heading}`)
                        .join("; ")}
                    </p>
                  </div>
                ))}

                {detail.spans.length > 0 && (
                  <section
                    className="help-citations"
                    aria-label={isPresentedAnswer ? "Cited answer spans" : "Match evidence"}
                  >
                    <strong>
                      {isPresentedAnswer
                        ? "Why this article is the answer"
                        : "Why this article matched"}
                    </strong>
                    <p className="help-citation-note">
                      Each quote below was re-resolved against the corpus before it was
                      shown, and names the documents backing that exact text.
                    </p>
                    <ul>
                      {detail.spans.map((span) => (
                        <li
                          key={`${span.field}:${span.passageId ?? ""}:${span.start}:${span.end}`}
                          className="help-citation"
                        >
                          <blockquote className="help-citation-quote">{span.quote}</blockquote>
                          <span className="help-citation-meta">
                            {span.field}
                            {span.passageId ? ` · ${span.passageId}` : ""} · matched “{span.term}”
                            {" · verified"}
                          </span>
                          <span className="help-citation-sources">
                            {span.sources
                              .map((source) => `${source.path} — ${source.heading}`)
                              .join("; ")}
                          </span>
                        </li>
                      ))}
                    </ul>
                    {detail.unverifiedSpanCount > 0 && (
                      <p className="help-citation-dropped" role="alert">
                        {detail.unverifiedSpanCount} span
                        {detail.unverifiedSpanCount === 1 ? " was" : "s were"} not shown:
                        the corpus did not reproduce the quoted text.
                      </p>
                    )}
                  </section>
                )}

                <div className="help-source-card">
                  <strong>Source-backed offline guidance</strong>
                  <span>
                    Product corpus v1 · {detail.articleId} · retrieval:{" "}
                    {view.retrievalMode} · corpus digest{" "}
                    <code className="help-digest">{view.digest}</code>
                  </span>
                  {view.query && detail.matchedTerms.length > 0 && (
                    <span>
                      Rank signal: {rankSignal(detail.confidence)} · query coverage{" "}
                      {rankSignal(detail.coverage)}
                      <span className="help-confidence-note">
                        {" "}
                        · ranking signal only, not certification
                      </span>
                    </span>
                  )}
                  <ul aria-label="Article sources">
                    {detail.sources.map((source) => (
                      <li key={source.id}>
                        <code>{source.path}</code> · {source.heading}
                      </li>
                    ))}
                  </ul>
                </div>

                <p className="help-index-note">
                  This came from the deterministic offline corpus; no model or network
                  was used to find it. Ranking is a pure function of the corpus and the
                  query, so the same question returns the same articles on every
                  machine.
                </p>

                {(onAnswer || onAskAssistant) && (
                  <section className="help-assistant-card" aria-label="Optional help assistant">
                    <strong>Ask the optional assistant</strong>
                    <p>
                      {onAnswer
                        ? "The cited articles this question retrieved would be offered to the configured provider — the confirmation lists exactly which."
                        : "Only this article\u2019s cited context would be offered to the configured provider."}{" "}
                      No workspace, session, credential, or clipboard data is included,
                      and nothing is sent until you confirm.
                    </p>

                    {onAnswer ? (
                      <>
                        {!view.canAskModel && (
                          <p className="help-assistant-blocked" role="note">
                            Unavailable for this result: Help abstained, and a model is
                            not asked to cover for a retriever that already said it did
                            not know.
                          </p>
                        )}
                        {view.canAskModel && (ask.status === "idle" || ask.status === "refused") && (
                          <button
                            type="button"
                            onClick={(event) => beginAnswerRequest(event.currentTarget)}
                          >
                            Prepare cited answer request
                          </button>
                        )}
                        {ask.status === "refused" && (
                          <p className="help-ask-refused" role="alert">
                            The cited answer seam refused this request:{" "}
                            <code>{ask.refusal}</code>. Nothing was sent.
                          </p>
                        )}
                        {ask.status === "pending" && (
                          <div className="help-ask-pending">
                            <p role="status">
                              Waiting for the configured provider… nothing has been
                              received yet.
                            </p>
                            <p className="help-ask-unknowns">
                              provider: {askUnknowns?.provider} · model:{" "}
                              {askUnknowns?.model} · cost: {askUnknowns?.cost} · latency:{" "}
                              {askUnknowns?.latency}
                            </p>
                            <button
                              type="button"
                              onClick={() => {
                                abortAsk();
                                setAsk({ status: "idle" });
                              }}
                            >
                              Cancel request
                            </button>
                          </div>
                        )}
                        {ask.status === "settled" && (
                          <div
                            className={`help-ask-result help-ask-${ask.summary.status}`}
                            role={ask.summary.status === "answered" ? "status" : "alert"}
                          >
                            <strong>{ask.summary.headline}</strong>
                            <p>{ask.summary.detail}</p>
                            {ask.response && ask.summary.status === "answered" && (
                              <>
                                <p className="help-ask-text">{ask.response.text}</p>
                                <small>Citations: {ask.response.citations.join(", ")}</small>
                                <small>Uncertainty: {ask.response.uncertainty}</small>
                              </>
                            )}
                            <small className="help-ask-unknowns">
                              provider: {askUnknowns?.provider} · model: {askUnknowns?.model}{" "}
                              · cost: {askUnknowns?.cost} · latency: {askUnknowns?.latency}
                            </small>
                            <small>{askUnknowns?.note}</small>
                          </div>
                        )}
                      </>
                    ) : (
                      <>
                        {(assistant.status === "idle" || assistant.status === "confirm") && (
                          <button
                            type="button"
                            onClick={(event) => beginAssistantRequest(event.currentTarget)}
                          >
                            Prepare cited question
                          </button>
                        )}
                        {assistant.status === "loading" && (
                          <p role="status">Waiting for the configured provider…</p>
                        )}
                        {assistant.status === "answer" && (
                          <div className="help-assistant-answer" role="status">
                            <strong>Draft answer — not product truth until reviewed</strong>
                            <p>{assistant.answer.text}</p>
                            <small>Citations: {assistant.answer.citations.join(", ")}</small>
                            <small>Uncertainty: {assistant.answer.uncertainty}</small>
                          </div>
                        )}
                        {assistant.status === "error" && (
                          <p role="alert">{assistant.message}</p>
                        )}
                      </>
                    )}
                  </section>
                )}
                {!onAnswer && !onAskAssistant && (
                  <p className="help-assistant-note" role="status">
                    Optional assistant: not connected in this build. Cited offline
                    guidance remains available without a provider.
                  </p>
                )}
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

      {topConfirm === "semantic" && rerank.status === "confirm" && (
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
              Send this query and {rerank.request.candidates.length} article title
              {rerank.request.candidates.length === 1 ? "" : "s"} and summar
              {rerank.request.candidates.length === 1 ? "y" : "ies"} to {providerLabel} to
              re-order these results? No article body or workspace data will be sent, and
              the provider cannot change whether Help answered or abstained.
            </p>
            <button type="button" className="primary" onClick={() => void confirmRerankRequest()}>
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
              Ready to send the cited article bundle ({assistant.request.sources.length} source
              {assistant.request.sources.length === 1 ? "" : "s"}) via {providerLabel}?
            </p>
            <button type="button" className="primary" onClick={() => void confirmAssistantRequest()}>
              Send cited context
            </button>
            <button type="button" onClick={() => dismissConfirm("assistant")}>
              Cancel
            </button>
          </div>
        </div>
      )}

      {topConfirm === "answer" && ask.status === "confirm" && (
        <div className="help-confirm-layer">
          <div
            ref={confirmDialogRef}
            className="help-answer-confirm"
            role="alertdialog"
            aria-modal="true"
            aria-labelledby="help-answer-confirm-title"
            aria-describedby="help-answer-confirm-copy"
          >
            <p>
              <strong id="help-answer-confirm-title">Confirm cited answer request</strong>
            </p>
            <p id="help-answer-confirm-copy">
              Send your question and {ask.request.citations.length} cited article
              {ask.request.citations.length === 1 ? "" : "s"} to {providerLabel}? The
              request carries no workspace, session, or credential data, grants no tools,
              stores nothing, and is abandoned after{" "}
              {ask.request.timeoutMs / 1_000}s.
            </p>
            <ul className="help-answer-bundle" aria-label="Articles in this request">
              {ask.request.citations.map((citation) => (
                <li key={citation.articleId}>
                  <code>{citation.articleId}</code> · {citation.sourceIds.join(", ")}
                </li>
              ))}
            </ul>
            <p className="help-ask-unknowns">
              Which provider and model serve this, and what it costs, are not known to
              Help: provider {askUnknowns?.provider} · model {askUnknowns?.model} · cost{" "}
              {askUnknowns?.cost}.
            </p>
            <button type="button" className="primary" onClick={() => void confirmAnswerRequest()}>
              Send cited context
            </button>
            <button type="button" onClick={() => dismissConfirm("answer")}>
              Cancel
            </button>
          </div>
        </div>
      )}
    </div>
  );
}
