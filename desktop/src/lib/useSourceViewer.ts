/**
 * Source-viewer state for the desktop shell.
 *
 * Holds the open request, the loaded document, and the refusal (if any), so
 * `App` can open a file from a file list, a tool path, or a diff row without
 * repeating the load-and-refuse dance three times.
 */

import { useCallback, useEffect, useRef, useState } from "react";
import { api } from "./api";
import { pickSourceRoot, type SourceDocument, type SourceRootPreference } from "./sourceView";

/** A pending or active request to view one file. */
export interface SourceViewRequest {
  path: string;
  line: number | null;
  preference: SourceRootPreference;
}

export interface SourceViewerState {
  open: boolean;
  request: SourceViewRequest | null;
  document: SourceDocument | null;
  error: unknown;
  loading: boolean;
  openSource: (path: string, line?: number | null, preference?: SourceRootPreference) => void;
  close: () => void;
  retry: () => void;
}

export function useSourceViewer(sessionId: string | null): SourceViewerState {
  const [request, setRequest] = useState<SourceViewRequest | null>(null);
  const [document, setDocument] = useState<SourceDocument | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [loading, setLoading] = useState(false);
  const [attempt, setAttempt] = useState(0);
  /** Guards against a slow first read landing after a second one. */
  const loadIdRef = useRef(0);

  const openSource = useCallback(
    (path: string, line: number | null = null, preference: SourceRootPreference = {}) => {
      setDocument(null);
      setError(null);
      setRequest({ path, line, preference });
      setAttempt((value) => value + 1);
    },
    [],
  );

  const close = useCallback(() => {
    loadIdRef.current += 1;
    setRequest(null);
    setDocument(null);
    setError(null);
    setLoading(false);
  }, []);

  const retry = useCallback(() => setAttempt((value) => value + 1), []);

  useEffect(() => {
    if (!request) return;
    let cancelled = false;
    loadIdRef.current += 1;
    const loadId = loadIdRef.current;
    setLoading(true);
    setError(null);

    void (async () => {
      try {
        const roots = await api.sourceViewRoots(sessionId);
        const root = pickSourceRoot(roots, request.preference);
        if (!root) {
          throw new Error(
            request.preference.runId
              ? "unknown_root: that run has no inspectable isolated worktree"
              : "no_approved_root: open a project folder first",
          );
        }
        const loaded = await api.sourceViewOpen(root.id, request.path, { sessionId });
        if (cancelled || loadId !== loadIdRef.current) return;
        setDocument(loaded);
      } catch (caught) {
        if (cancelled || loadId !== loadIdRef.current) return;
        setDocument(null);
        setError(caught);
      } finally {
        if (!cancelled && loadId === loadIdRef.current) setLoading(false);
      }
    })();

    return () => {
      cancelled = true;
    };
  }, [request, attempt, sessionId]);

  return {
    open: request !== null,
    request,
    document,
    error,
    loading,
    openSource,
    close,
    retry,
  };
}
