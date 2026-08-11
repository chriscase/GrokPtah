import { useEffect, useState } from "react";
import type { PromptQueueEntry } from "../lib/promptQueue";

type PromptQueuePanelProps = {
  entries: PromptQueueEntry[];
  busy: boolean;
  onEdit: (entry: PromptQueueEntry, text: string) => Promise<void>;
  onRemove: (entry: PromptQueueEntry) => Promise<void>;
  onClear: () => Promise<void>;
  onMove: (entry: PromptQueueEntry, toIndex: number) => Promise<void>;
  onSteer: (entry: PromptQueueEntry) => Promise<void>;
  onRunNext: (entry: PromptQueueEntry) => Promise<void>;
  onError?: (message: string) => void;
};

function queueState(entry: PromptQueueEntry) {
  if (entry.priority) {
    return { label: "Run next", className: "run-next" };
  }
  if (entry.source === "steer_now") {
    return { label: "Steering pending", className: "steering-pending" };
  }
  if (entry.source === "steering_deferred") {
    return { label: "Steering deferred", className: "steering-deferred" };
  }
  return { label: "Queued", className: "queued" };
}

function PromptQueueRow({
  entry,
  index,
  count,
  busy,
  onEdit,
  onRemove,
  onMove,
  onSteer,
  onRunNext,
  onError,
}: {
  entry: PromptQueueEntry;
  index: number;
  count: number;
  busy: boolean;
  onEdit: PromptQueuePanelProps["onEdit"];
  onRemove: PromptQueuePanelProps["onRemove"];
  onMove: PromptQueuePanelProps["onMove"];
  onSteer: PromptQueuePanelProps["onSteer"];
  onRunNext: PromptQueuePanelProps["onRunNext"];
  onError?: PromptQueuePanelProps["onError"];
}) {
  const [draft, setDraft] = useState(entry.text);
  const [workingAction, setWorkingAction] = useState<string | null>(null);
  const [error, setError] = useState(false);

  useEffect(() => {
    setDraft(entry.text);
  }, [entry.id, entry.text, entry.version]);

  async function run(label: string, action: () => Promise<void>) {
    if (workingAction) return;
    setWorkingAction(label);
    setError(false);
    try {
      await action();
    } catch (cause) {
      setError(true);
      onError?.(cause instanceof Error ? cause.message : "Queue update failed");
    } finally {
      setWorkingAction(null);
    }
  }

  async function commitEdit() {
    const text = draft.trim();
    if (!text) {
      setDraft(entry.text);
      return;
    }
    if (text !== entry.text) {
      await run("Saving", () => onEdit(entry, text));
    }
  }

  const state = queueState(entry);
  const working = workingAction !== null;

  return (
    <li className={`prompt-queue-row ${entry.priority ? "is-priority" : ""}`}>
      <div className="prompt-queue-order" aria-label={`Queue position ${index + 1}`}>
        {index + 1}
      </div>
      <div className="prompt-queue-content">
        <textarea
          className="prompt-queue-edit"
          aria-label={`Queued prompt ${index + 1}`}
          aria-invalid={error}
          rows={2}
          value={draft}
          disabled={working}
          onChange={(event) => setDraft(event.target.value)}
          onBlur={() => void commitEdit()}
          onKeyDown={(event) => {
            if ((event.metaKey || event.ctrlKey) && event.key === "Enter") {
              event.preventDefault();
              void commitEdit();
            }
          }}
        />
        <div className="prompt-queue-meta" aria-live="polite">
          <span className={`prompt-queue-state ${state.className}`}>
            {workingAction ?? state.label}
          </span>
          {error && <span className="prompt-queue-error">Update failed</span>}
        </div>
      </div>
      <div className="prompt-queue-actions">
        <button
          type="button"
          className="prompt-queue-icon"
          title="Move up"
          aria-label={`Move queued prompt ${index + 1} up`}
          disabled={working || index === 0}
          onClick={() => void run("Moving", () => onMove(entry, index - 1))}
        >
          ↑
        </button>
        <button
          type="button"
          className="prompt-queue-icon"
          title="Move down"
          aria-label={`Move queued prompt ${index + 1} down`}
          disabled={working || index === count - 1}
          onClick={() => void run("Moving", () => onMove(entry, index + 1))}
        >
          ↓
        </button>
        <button
          type="button"
          className="prompt-queue-command"
          title={
            busy
              ? "Guide the running agent at its next safe step without stopping it"
              : "No turn is running; keep this prompt queued to run next"
          }
          disabled={working}
          onClick={() => void run("Steering", () => onSteer(entry))}
        >
          Steer now
        </button>
        <button
          type="button"
          className="prompt-queue-command is-strong"
          title="Make this the next prompt; stops the current turn when one is running"
          disabled={working}
          onClick={() => void run("Promoting", () => onRunNext(entry))}
        >
          Run next
        </button>
        <button
          type="button"
          className="prompt-queue-icon is-remove"
          title="Remove from queue"
          aria-label={`Remove queued prompt ${index + 1}`}
          disabled={working}
          onClick={() => void run("Removing", () => onRemove(entry))}
        >
          ×
        </button>
      </div>
    </li>
  );
}

export function PromptQueuePanel({
  entries,
  busy,
  onEdit,
  onRemove,
  onClear,
  onMove,
  onSteer,
  onRunNext,
  onError,
}: PromptQueuePanelProps) {
  const [clearing, setClearing] = useState(false);
  if (entries.length === 0) return null;

  return (
    <section className="prompt-queue-panel" aria-label="Queued prompts">
      <div className="prompt-queue-header">
        <span>Queue</span>
        <span className="prompt-queue-count">{entries.length}</span>
        <span className="prompt-queue-guidance">
          Steering guides the current turn without stopping it.
        </span>
        <button
          type="button"
          className="prompt-queue-clear"
          disabled={clearing}
          onClick={() => {
            setClearing(true);
            void onClear()
              .catch((cause) => {
                onError?.(
                  cause instanceof Error ? cause.message : "Queue clear failed",
                );
              })
              .finally(() => setClearing(false));
          }}
        >
          Clear
        </button>
      </div>
      <ol className="prompt-queue-list">
        {entries.map((entry, index) => (
          <PromptQueueRow
            key={entry.id}
            entry={entry}
            index={index}
            count={entries.length}
            busy={busy}
            onEdit={onEdit}
            onRemove={onRemove}
            onMove={onMove}
            onSteer={onSteer}
            onRunNext={onRunNext}
            onError={onError}
          />
        ))}
      </ol>
    </section>
  );
}
