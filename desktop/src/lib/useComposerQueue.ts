import {
  type Dispatch,
  useCallback,
  useEffect,
  useReducer,
  useRef,
  useState,
} from "react";
import {
  emptyPromptQueueState,
  promptQueueReducer,
  queueEntriesFor,
  type PromptQueueAction,
  type PromptQueueEntry,
} from "./promptQueue";

export function useComposerQueue(activeSessionId: string | null) {
  const [composer, setComposer] = useState("");
  const [drafts, setDrafts] = useState<Record<string, string>>({});
  const [expanded, setExpanded] = useState(false);
  const queueRequestVersions = useRef(new Map<string, number>());
  const [queues, dispatchQueue] = useReducer(
    promptQueueReducer,
    emptyPromptQueueState,
  );

  const invalidateQueue = useCallback((sessionId: string) => {
    const next = (queueRequestVersions.current.get(sessionId) ?? 0) + 1;
    queueRequestVersions.current.set(sessionId, next);
    return next;
  }, []);

  const isCurrentQueueRequest = useCallback(
    (sessionId: string, version: number) =>
      queueRequestVersions.current.get(sessionId) === version,
    [],
  );

  /**
   * Apply a bridge snapshot only if it is still the newest request for the
   * session. Startup hydration and a fast edit/reorder can otherwise race.
   */
  const syncQueue = useCallback(
    async (
      sessionId: string,
      load: () => Promise<PromptQueueEntry[]>,
    ): Promise<PromptQueueEntry[]> => {
      const version = invalidateQueue(sessionId);
      const entries = await load();
      if (isCurrentQueueRequest(sessionId, version)) {
        dispatchQueue({ type: "replace", sessionId, entries });
      }
      return entries;
    },
    [invalidateQueue, isCurrentQueueRequest],
  );

  const updateComposer = useCallback(
    (text: string) => {
      setComposer(text);
      if (!activeSessionId) return;
      setDrafts((current) => {
        if ((current[activeSessionId] ?? "") === text) return current;
        if (!text) {
          if (!(activeSessionId in current)) return current;
          const next = { ...current };
          delete next[activeSessionId];
          return next;
        }
        return { ...current, [activeSessionId]: text };
      });
    },
    [activeSessionId],
  );

  const clearComposerFor = useCallback((...sessionIds: (string | null)[]) => {
    setComposer("");
    setDrafts((current) => {
      const next = { ...current };
      let changed = false;
      for (const sessionId of sessionIds) {
        if (sessionId && sessionId in next) {
          delete next[sessionId];
          changed = true;
        }
      }
      return changed ? next : current;
    });
  }, []);

  const restoreComposer = useCallback(
    (text: string, sessionId?: string | null) => {
      setComposer(text);
      const target = sessionId ?? activeSessionId;
      if (target) {
        setDrafts((current) => ({ ...current, [target]: text }));
      }
    },
    [activeSessionId],
  );

  const forgetSession = useCallback((sessionId: string) => {
    setDrafts((current) => {
      if (!(sessionId in current)) return current;
      const next = { ...current };
      delete next[sessionId];
      return next;
    });
  }, []);

  useEffect(() => {
    setComposer(activeSessionId ? (drafts[activeSessionId] ?? "") : "");
    // Draft changes are already mirrored by updateComposer; this effect only
    // rehydrates when focus moves to another session.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeSessionId]);

  return {
    composer,
    updateComposer,
    clearComposerFor,
    restoreComposer,
    forgetSession,
    expanded,
    setExpanded,
    queues,
    dispatchQueue: dispatchQueue as Dispatch<PromptQueueAction>,
    invalidateQueue,
    isCurrentQueueRequest,
    syncQueue,
    queueFor: (sessionId: string | null): PromptQueueEntry[] =>
      queueEntriesFor(queues, sessionId),
  };
}
