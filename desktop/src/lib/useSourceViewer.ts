/**
 * Source-viewer state for the desktop shell.
 *
 * Holds the authorization snapshot, the one root a request names, the pages
 * loaded so far, and any refusal. The flow is always the same and never
 * shortcuts:
 *
 *   snapshot → select exactly one root → read → page with its cursor
 *
 * A refusal about *authorization* re-issues the snapshot once and retries;
 * anything else is surfaced. Nothing in here mutates durable state, so a
 * viewer failure can never change what a reviewer is allowed to promote.
 */

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { tauriSourceViewTransport } from "./api";
import {
  appendSourceChunk,
  isAuthorizationRefusal,
  selectSourceRoot,
  shouldRefreshSnapshot,
  type SourceDocument,
  type SourceLine,
  type SourceRootDescriptor,
  type SourceRootSelector,
  type SourceRootSnapshot,
} from "./sourceView";
import type { SourceViewTransport } from "./sourceViewTransport";

/** A pending or active request to view one file. */
export interface SourceViewRequest {
  path: string;
  line: number | null;
  selector: SourceRootSelector;
}

/** Why the viewer cannot proceed without the reader choosing. */
export interface SourceRootChoice {
  candidates: SourceRootDescriptor[];
}

export interface SourceViewerState {
  open: boolean;
  request: SourceViewRequest | null;
  /** The most recent page; carries identity, limits, and classification. */
  document: SourceDocument | null;
  /** Every line loaded so far, with continued lines rejoined. */
  lines: SourceLine[];
  snapshot: SourceRootSnapshot | null;
  error: unknown;
  loading: boolean;
  loadingMore: boolean;
  /** Present when a selector matched more than one root. */
  choice: SourceRootChoice | null;
  hasMore: boolean;
  openSource: (path: string, line?: number | null, selector?: SourceRootSelector) => void;
  chooseRoot: (token: string) => void;
  loadMore: () => void;
  close: () => void;
  retry: () => void;
}

export interface SourceViewerOptions {
  transport?: SourceViewTransport;
  now?: () => number;
}

export function useSourceViewer(
  sessionId: string | null,
  options: SourceViewerOptions = {},
): SourceViewerState {
  /**
   * Options arrive as a fresh object literal on every render, so holding them
   * in refs is what keeps the read effect from re-running forever. An effect
   * that depends on a callback the caller re-creates each render is an
   * infinite loop with extra steps.
   */
  const transportRef = useRef(options.transport ?? tauriSourceViewTransport);
  transportRef.current = options.transport ?? tauriSourceViewTransport;
  const nowRef = useRef(options.now ?? (() => Date.now()));
  nowRef.current = options.now ?? (() => Date.now());

  const [request, setRequest] = useState<SourceViewRequest | null>(null);
  const [snapshot, setSnapshot] = useState<SourceRootSnapshot | null>(null);
  const [document, setDocument] = useState<SourceDocument | null>(null);
  const [lines, setLines] = useState<SourceLine[]>([]);
  const [error, setError] = useState<unknown>(null);
  const [loading, setLoading] = useState(false);
  const [loadingMore, setLoadingMore] = useState(false);
  const [choice, setChoice] = useState<SourceRootChoice | null>(null);
  const [attempt, setAttempt] = useState(0);

  /** Guards against a slow read landing after a newer one. */
  const generation = useRef(0);
  const snapshotRef = useRef<SourceRootSnapshot | null>(null);
  snapshotRef.current = snapshot;

  const openSource = useCallback(
    (
      path: string,
      line: number | null = null,
      selector: SourceRootSelector = { by: "workspace" },
    ) => {
      generation.current += 1;
      setDocument(null);
      setLines([]);
      setError(null);
      setChoice(null);
      setRequest({ path, line, selector });
      setAttempt((value) => value + 1);
    },
    [],
  );

  const chooseRoot = useCallback((token: string) => {
    setChoice(null);
    setError(null);
    setRequest((current) => (current ? { ...current, selector: { by: "token", token } } : current));
    setAttempt((value) => value + 1);
  }, []);

  const close = useCallback(() => {
    generation.current += 1;
    setRequest(null);
    setDocument(null);
    setLines([]);
    setError(null);
    setChoice(null);
    setLoading(false);
    setLoadingMore(false);
  }, []);

  const retry = useCallback(() => setAttempt((value) => value + 1), []);

  /** Obtain a snapshot, reusing a live one unless it is close to expiring. */
  const ensureSnapshot = useCallback(
    async (force: boolean): Promise<SourceRootSnapshot> => {
      const current = snapshotRef.current;
      if (!force && !shouldRefreshSnapshot(current, nowRef.current())) {
        return current as SourceRootSnapshot;
      }
      const issued = await transportRef.current.snapshot({ sessionId });
      snapshotRef.current = issued;
      setSnapshot(issued);
      return issued;
    },
    [sessionId],
  );

  // Initial read for the active request.
  useEffect(() => {
    if (!request) return;
    let cancelled = false;
    generation.current += 1;
    const mine = generation.current;
    setLoading(true);
    setError(null);

    void (async () => {
      /**
       * One attempt. `force` re-issues the snapshot first.
       *
       * Ambiguity is returned rather than thrown: it is an answer the reader
       * must resolve, not a failure to retry against a fresh snapshot.
       */
      const attemptRead = async (
        force: boolean,
      ): Promise<
        | { status: "read"; document: SourceDocument }
        | { status: "ambiguous"; candidates: SourceRootDescriptor[] }
      > => {
        const issued = await ensureSnapshot(force);
        const selection = selectSourceRoot(issued, request.selector);
        if (selection.kind === "ambiguous") {
          return { status: "ambiguous", candidates: selection.candidates };
        }
        if (selection.kind === "absent") {
          throw new Error(
            request.selector.by === "run"
              ? "unknown_root: that run has no inspectable isolated worktree"
              : "no_approved_root: no approved workspace matched this request",
          );
        }
        return {
          status: "read",
          document: await transportRef.current.read({
            token: selection.root.token,
            path: request.path,
            sessionId,
          }),
        };
      };

      try {
        let outcome: Awaited<ReturnType<typeof attemptRead>>;
        try {
          outcome = await attemptRead(false);
        } catch (caught) {
          // An authorization refusal means the snapshot is stale. Re-issue
          // once and retry; a second refusal is real and is surfaced.
          if (!isAuthorizationRefusal(caught)) throw caught;
          outcome = await attemptRead(true);
        }
        if (cancelled || mine !== generation.current) return;
        if (outcome.status === "ambiguous") {
          setChoice({ candidates: outcome.candidates });
          setDocument(null);
          setLines([]);
          return;
        }
        setDocument(outcome.document);
        setLines(appendSourceChunk([], outcome.document.chunk));
        setChoice(null);
      } catch (caught) {
        if (cancelled || mine !== generation.current) return;
        setDocument(null);
        setLines([]);
        setError(caught);
      } finally {
        if (!cancelled && mine === generation.current) setLoading(false);
      }
    })();

    return () => {
      cancelled = true;
    };
  }, [request, attempt, sessionId, ensureSnapshot]);

  const loadMore = useCallback(() => {
    const current = document;
    const cursor = current?.chunk.nextCursor;
    if (!current || !cursor || !request) return;
    let cancelled = false;
    const mine = generation.current;
    setLoadingMore(true);

    void (async () => {
      try {
        const next = await transportRef.current.read({
          token: current.root.token,
          path: current.relativePath,
          sessionId,
          cursor,
        });
        if (cancelled || mine !== generation.current) return;
        setDocument(next);
        setLines((existing) => appendSourceChunk(existing, next.chunk));
      } catch (caught) {
        if (cancelled || mine !== generation.current) return;
        // Paging failed; the pages already loaded stay readable and nothing
        // durable is touched.
        setError(caught);
      } finally {
        if (!cancelled && mine === generation.current) setLoadingMore(false);
      }
    })();
  }, [document, request, sessionId]);

  const hasMore = useMemo(
    () => Boolean(document && !document.chunk.eof && document.chunk.nextCursor),
    [document],
  );

  return {
    open: request !== null,
    request,
    document,
    lines,
    snapshot,
    error,
    loading,
    loadingMore,
    choice,
    hasMore,
    openSource,
    chooseRoot,
    loadMore,
    close,
    retry,
  };
}
