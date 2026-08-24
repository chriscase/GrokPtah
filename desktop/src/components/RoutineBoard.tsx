import { useMemo, useState } from "react";
import { safeErrorMessage } from "../lib/errorMessage";
import type { DurableRoutine, RemoteRoutineSnapshot } from "../lib/protocol";
import { StateCard } from "./StateCard";

export type RoutineBoardProps = {
  items: DurableRoutine[];
  selectedRoutineId: string | null;
  snapshot: RemoteRoutineSnapshot | null;
  busy?: boolean;
  error?: string | null;
  mutationsEnabled?: boolean;
  defaultAgentId?: string;
  onRefresh: () => void;
  onSelect: (routineId: string) => void;
  onCreate?: (name: string, agentId: string, objective: string) => Promise<void>;
  onSetLifecycle?: (
    routineId: string,
    lifecycle: "enabled" | "paused" | "disabled",
    expectedRevision: number,
  ) => Promise<void>;
  onFire?: (routineId: string) => Promise<void>;
};

export function RoutineBoard({
  items,
  selectedRoutineId,
  snapshot,
  busy = false,
  error,
  mutationsEnabled = false,
  defaultAgentId = "",
  onRefresh,
  onSelect,
  onCreate,
  onSetLifecycle,
  onFire,
}: RoutineBoardProps) {
  const [name, setName] = useState("manual-check");
  const [agentId, setAgentId] = useState(defaultAgentId);
  const [objective, setObjective] = useState("Create eligible Work from this routine");
  const [actionError, setActionError] = useState<string | null>(null);
  const selected =
    snapshot?.routine ?? items.find((item) => item.routineId === selectedRoutineId) ?? null;
  const activations = snapshot?.activations ?? [];
  const visible = useMemo(() => items, [items]);

  async function run(action: () => Promise<void>) {
    setActionError(null);
    try {
      await action();
    } catch (cause) {
      setActionError(safeErrorMessage(cause));
    }
  }

  return (
    <section className="routine-board" aria-label="Durable routines">
      <header className="work-board-header">
        <div>
          <h3>Routines</h3>
          <p>
            The runtime-home owner fires schedules. This view only displays
            state and requests create, fire, pause, enable, or disable.
          </p>
        </div>
        <button type="button" onClick={onRefresh} disabled={busy}>
          Refresh
        </button>
      </header>
      {error && (
        <StateCard variant="error" title="Routine state unavailable" description={error} />
      )}
      {actionError && (
        <StateCard variant="error" title="Routine action failed" description={actionError} />
      )}
      {mutationsEnabled && onCreate && (
        <form
          className="work-create"
          onSubmit={(event) => {
            event.preventDefault();
            void run(() => onCreate(name, agentId, objective));
          }}
        >
          <input
            aria-label="Routine name"
            value={name}
            onChange={(event) => setName(event.target.value)}
          />
          <input
            aria-label="Owning agent id"
            value={agentId}
            onChange={(event) => setAgentId(event.target.value)}
            placeholder="agent id"
          />
          <input
            aria-label="Work objective"
            value={objective}
            onChange={(event) => setObjective(event.target.value)}
          />
          <button type="submit">Create manual routine</button>
        </form>
      )}
      <ul className="work-list">
        {visible.map((item) => (
          <li key={item.routineId}>
            <button
              type="button"
              className={item.routineId === selected?.routineId ? "selected" : undefined}
              onClick={() => onSelect(item.routineId)}
            >
              {item.name} · {item.lifecycle} · {item.trigger.kind}
            </button>
          </li>
        ))}
      </ul>
      {selected && (
        <article className="work-detail">
          <h4>{selected.name}</h4>
          <p>Lifecycle: {selected.lifecycle}</p>
          <p>Next fire: {selected.nextFireAt ?? "none"}</p>
          <p>Circuit: {selected.circuitOpen ? "open" : "closed"}</p>
          {mutationsEnabled && (
            <div className="work-actions">
              {onFire && (
                <button type="button" onClick={() => void run(() => onFire(selected.routineId))}>
                  Fire now
                </button>
              )}
              {onSetLifecycle && (
                <>
                  <button
                    type="button"
                    onClick={() =>
                      void run(() => onSetLifecycle(selected.routineId, "paused", selected.revision))
                    }
                  >
                    Pause
                  </button>
                  <button
                    type="button"
                    onClick={() =>
                      void run(() =>
                        onSetLifecycle(selected.routineId, "enabled", selected.revision),
                      )
                    }
                  >
                    Enable
                  </button>
                  <button
                    type="button"
                    onClick={() =>
                      void run(() =>
                        onSetLifecycle(selected.routineId, "disabled", selected.revision),
                      )
                    }
                  >
                    Disable
                  </button>
                </>
              )}
            </div>
          )}
          <h5>Activation history</h5>
          <ul>
            {activations.map((activation) => (
              <li key={activation.activationId}>
                {activation.disposition}
                {activation.workId ? ` · work ${activation.workId}` : ""}
              </li>
            ))}
          </ul>
        </article>
      )}
    </section>
  );
}
