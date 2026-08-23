import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api } from "../lib/api";
import type { ComputerCockpitSnapshot, ComputerLocalApproval } from "../lib/protocol";

export type AppOwnedComputerRun = {
  sessionId: string;
  sessionTitle?: string;
  snapshot: ComputerCockpitSnapshot;
};

type PersistentComputerRunsProps = {
  runs: AppOwnedComputerRun[];
  preferredSessionId?: string | null;
  onSnapshot: (sessionId: string, snapshot: ComputerCockpitSnapshot) => void;
  onOpen: (sessionId: string) => void;
};

const TERMINAL_STATES = new Set([
  "completed",
  "failed",
  "cancelled",
  "interrupted",
  "limit_reached",
]);

function isLive(run: ComputerLocalApproval | null | undefined) {
  return Boolean(run && !TERMINAL_STATES.has(run.state));
}

function titleCase(value: string) {
  return value.replaceAll("_", " ").replace(/\b\w/g, (char) => char.toUpperCase());
}

export function PersistentComputerRuns({
  runs,
  preferredSessionId,
  onSnapshot,
  onOpen,
}: PersistentComputerRunsProps) {
  const active = useMemo(
    () => runs.filter((binding) => isLive(binding.snapshot.local)),
    [runs],
  );
  const [notice, setNotice] = useState<string | null>(null);
  const operationEpoch = useRef(new Map<string, number>());

  const applyEmergency = useCallback(
    async (
      binding: AppOwnedComputerRun,
      kind: "pause" | "takeover" | "stop",
    ) => {
      const run = binding.snapshot.local;
      if (!run || !isLive(run)) return;
      const key = `${binding.sessionId}:${run.runId}`;
      const epoch = (operationEpoch.current.get(key) ?? 0) + 1;
      operationEpoch.current.set(key, epoch);
      setNotice(null);
      try {
        const next = await (kind === "pause"
          ? api.computerUseCockpitPause(binding.sessionId, run.runId)
          : kind === "takeover"
            ? api.computerUseCockpitTakeOver(binding.sessionId, run.runId)
            : api.computerUseCockpitStop(binding.sessionId, run.runId));
        if (operationEpoch.current.get(key) !== epoch) return;
        onSnapshot(binding.sessionId, next);
        setNotice(
          kind === "stop"
            ? `${run.target.displayName} stopped.`
            : kind === "takeover"
              ? `You now control ${run.target.displayName}.`
              : `${run.target.displayName} paused.`,
        );
      } catch (error) {
        if (operationEpoch.current.get(key) === epoch) setNotice(String(error));
      }
    },
    [onSnapshot],
  );

  const shortcutTarget = useMemo(
    () =>
      active.find((binding) => binding.sessionId === preferredSessionId) ??
      (active.length === 1 ? active[0] : null),
    [active, preferredSessionId],
  );

  useEffect(() => {
    if (!shortcutTarget) return;
    const onKey = (event: KeyboardEvent) => {
      if (!event.ctrlKey || !event.shiftKey || event.altKey || event.metaKey) return;
      const run = shortcutTarget.snapshot.local;
      if (!run) return;
      const key = event.key.toLowerCase();
      if (key === "s") {
        event.preventDefault();
        void applyEmergency(shortcutTarget, "stop");
      } else if (key === "t" && run.controlDisposition !== "operator_takeover") {
        event.preventDefault();
        void applyEmergency(shortcutTarget, "takeover");
      } else if (key === "p" && run.state !== "paused") {
        event.preventDefault();
        void applyEmergency(shortcutTarget, "pause");
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [applyEmergency, shortcutTarget]);

  if (active.length === 0) return null;

  return (
    <aside className="persistent-computer-runs" aria-label="Active Computer Runs">
      <div className="persistent-computer-heading">
        <span className="computer-chrome-dot" aria-hidden />
        <strong>{active.length === 1 ? "Computer Run active" : `${active.length} Computer Runs active`}</strong>
      </div>
      <div className="persistent-computer-list">
        {active.map((binding) => {
          const run = binding.snapshot.local!;
          const hasTakeover = run.controlDisposition === "operator_takeover";
          return (
            <section
              key={`${binding.sessionId}:${run.runId}`}
              className="persistent-computer-run"
              aria-label={`${run.target.displayName} Computer Run`}
            >
              <button
                type="button"
                className="persistent-computer-identity"
                onClick={() => onOpen(binding.sessionId)}
                title="Open this exact Computer Run"
              >
                <strong>{run.target.displayName}</strong>
                <span>{binding.sessionTitle ?? binding.sessionId} · {titleCase(run.state)}</span>
              </button>
              <div className="persistent-computer-actions">
                <button
                  type="button"
                  disabled={run.state === "paused"}
                  aria-keyshortcuts="Control+Shift+P"
                  title="Pause now · Control+Shift+P"
                  onClick={() => void applyEmergency(binding, "pause")}
                >
                  Pause
                </button>
                <button
                  type="button"
                  disabled={hasTakeover}
                  aria-keyshortcuts="Control+Shift+T"
                  title="Take over now · Control+Shift+T"
                  onClick={() => void applyEmergency(binding, "takeover")}
                >
                  Take over
                </button>
                <button
                  type="button"
                  className="danger"
                  aria-keyshortcuts="Control+Shift+S"
                  title="Stop now · Control+Shift+S"
                  onClick={() => void applyEmergency(binding, "stop")}
                >
                  Stop
                </button>
              </div>
            </section>
          );
        })}
      </div>
      <p className="computer-visually-hidden" role="status" aria-live="polite">
        {notice ?? ""}
      </p>
    </aside>
  );
}
