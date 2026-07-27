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
};

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
}) {
  const [draft, setDraft] = useState(entry.text);
  const [working, setWorking] = useState(false);

  useEffect(() => {
    setDraft(entry.text);
  }, [entry.id, entry.text, entry.version]);

  async function run(action: () => Promise<void>) {
    if (working) return;
    setWorking(true);
    try {
      await action();
    } finally {
      setWorking(false);
    }
  }

  async function commitEdit() {
    const text = draft.trim();
    if (!text) {
      setDraft(entry.text);
      return;
    }
    if (text !== entry.text) {
      await run(() => onEdit(entry, text));
    }
  }

  return (
    <li className={`prompt-queue-row ${entry.priority ? "is-priority" : ""}`}>
      <div className="prompt-queue-order" aria-label={`Queue position ${index + 1}`}>
        {index + 1}
      </div>
      <textarea
        className="prompt-queue-edit"
        aria-label={`Queued prompt ${index + 1}`}
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
      <div className="prompt-queue-actions">
        <button
          type="button"
          className="prompt-queue-icon"
          title="Move up"
          aria-label={`Move queued prompt ${index + 1} up`}
          disabled={working || index === 0}
          onClick={() => void run(() => onMove(entry, index - 1))}
        >
          ↑
        </button>
        <button
          type="button"
          className="prompt-queue-icon"
          title="Move down"
          aria-label={`Move queued prompt ${index + 1} down`}
          disabled={working || index === count - 1}
          onClick={() => void run(() => onMove(entry, index + 1))}
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
          onClick={() => void run(() => onSteer(entry))}
        >
          Steer now
        </button>
        <button
          type="button"
          className="prompt-queue-command is-strong"
          title="Make this the next prompt; stops the current turn when one is running"
          disabled={working}
          onClick={() => void run(() => onRunNext(entry))}
        >
          Run next
        </button>
        <button
          type="button"
          className="prompt-queue-icon is-remove"
          title="Remove from queue"
          aria-label={`Remove queued prompt ${index + 1}`}
          disabled={working}
          onClick={() => void run(() => onRemove(entry))}
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
            void onClear().finally(() => setClearing(false));
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
          />
        ))}
      </ol>
    </section>
  );
}
