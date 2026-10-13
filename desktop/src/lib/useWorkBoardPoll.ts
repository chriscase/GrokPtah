import { useCallback, useEffect, useRef, useState } from "react";
import {
  WORK_BOARD_POLL_MS,
  WorkBoardController,
  workBoardScopeKey,
  type WorkBoardPage,
  type WorkBoardScope,
  type WorkBoardView,
} from "./workBoardController";
import type { RemoteWorkSnapshot } from "./protocol";

export interface WorkBoardPollClient {
  list: (scope: WorkBoardScope, cursor: string | null) => Promise<WorkBoardPage>;
  get: (scope: WorkBoardScope, workId: string) => Promise<RemoteWorkSnapshot | null>;
}

/**
 * Production poll for the Work Board.
 * The interval calls `observe`, which holds an established continuation.
 * `restart` is the explicit Refresh / Retry / post-mutation path.
 */
export function useWorkBoardPoll(options: {
  enabled: boolean;
  scope: WorkBoardScope;
  client: WorkBoardPollClient;
  onRoutines?: () => void;
}) {
  const boardRef = useRef<WorkBoardController | null>(null);
  const listRef = useRef(options.client.list);
  const getRef = useRef(options.client.get);
  listRef.current = options.client.list;
  getRef.current = options.client.get;
  if (!boardRef.current) {
    boardRef.current = new WorkBoardController({
      list: (scope, cursor) => listRef.current(scope, cursor),
      get: (scope, workId) => getRef.current(scope, workId),
    });
  }
  const board = boardRef.current;
  const [view, setView] = useState<WorkBoardView>(() => board.snapshot());
  const scopeRef = useRef(options.scope);
  scopeRef.current = options.scope;
  const routinesRef = useRef(options.onRoutines);
  routinesRef.current = options.onRoutines;
  const scopeKey = workBoardScopeKey(options.scope);

  useEffect(() => {
    board.setListener(setView);
    return () => board.setListener(null);
  }, [board]);

  useEffect(() => {
    if (!options.enabled) {
      board.invalidate();
      return;
    }
    void board.observe(scopeRef.current);
    routinesRef.current?.();
    const timer = window.setInterval(() => {
      void board.observe(scopeRef.current);
      routinesRef.current?.();
    }, WORK_BOARD_POLL_MS);
    return () => {
      window.clearInterval(timer);
      board.invalidate();
    };
  }, [board, options.enabled, scopeKey, options.scope.remoteConnected, options.scope.remoteLanePresent]);

  const restart = useCallback(() => board.restart(scopeRef.current), [board]);
  const loadMore = useCallback(() => board.loadMore(scopeRef.current), [board]);
  const refreshDetail = useCallback(
    (workId: string) => board.refreshDetail(scopeRef.current, workId),
    [board],
  );
  const select = useCallback(
    (workId: string | null) => {
      board.select(workId);
    },
    [board],
  );
  const dismissDetail = useCallback(() => {
    board.dismissDetail();
  }, [board]);
  const observe = useCallback(() => board.observe(scopeRef.current), [board]);

  return { view, observe, restart, loadMore, refreshDetail, select, dismissDetail };
}
