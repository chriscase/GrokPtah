import type { PromptQueueEntry } from "../lib/promptQueue";

export function PromptQueueSummary({
  entries,
}: {
  entries: PromptQueueEntry[];
}) {
  if (entries.length === 0) return null;
  return (
    <span
      className="composer-hint"
      title="Prompts waiting for the current turn to finish"
    >
      Queue {entries.length}
    </span>
  );
}
