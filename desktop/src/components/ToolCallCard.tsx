import { useState } from "react";
import type { TranscriptItem } from "../lib/protocol";

type ToolItem = Extract<TranscriptItem, { kind: "tool" }>;

function statusGlyph(status: string): string {
  switch (status) {
    case "completed":
      return "✓";
    case "failed":
      return "✗";
    case "denied":
      return "⊘";
    case "running":
    case "pending":
      return "·";
    default:
      return "·";
  }
}

function shortOutput(output?: string, max = 96): string {
  if (!output) return "";
  const one = output.replace(/\s+/g, " ").trim();
  if (one.length <= max) return one;
  return `${one.slice(0, max)}…`;
}

/**
 * Compact one-line tool call; output expanded only on demand.
 * Running tools stay slightly open for live feedback.
 */
export function ToolCallCard({
  item,
  defaultOpen,
}: {
  item: ToolItem;
  defaultOpen?: boolean;
}) {
  const live = item.status === "running" || item.status === "pending";
  const [open, setOpen] = useState(Boolean(defaultOpen) || live);
  const preview = shortOutput(item.output);

  return (
    <div
      className={`tool-card status-${item.status} ${open ? "is-open" : ""} ${live ? "is-live" : ""}`}
    >
      <button
        type="button"
        className="tool-card-header"
        onClick={() => setOpen((v) => !v)}
        aria-expanded={open}
      >
        <span className="tool-card-glyph" aria-hidden>
          {statusGlyph(item.status)}
        </span>
        <span className="tool-card-title">{item.title}</span>
        <span className="tool-card-status">{item.status}</span>
        {!open && preview && (
          <span className="tool-card-preview" title={item.output}>
            {preview}
          </span>
        )}
        <span className="tool-card-chevron" aria-hidden>
          {open ? "▾" : "▸"}
        </span>
      </button>
      {open && item.output && (
        <pre className="tool-card-output">{item.output}</pre>
      )}
      {open && !item.output && live && (
        <div className="tool-card-waiting">Running…</div>
      )}
    </div>
  );
}

/** Collapse older tool calls into a single expandable group. */
export function ToolHistoryGroup({
  tools,
  keepRecent = 4,
}: {
  tools: { item: ToolItem; index: number }[];
  keepRecent?: number;
}) {
  const [showOlder, setShowOlder] = useState(false);
  if (tools.length === 0) return null;

  const older =
    tools.length > keepRecent ? tools.slice(0, tools.length - keepRecent) : [];
  const recent =
    tools.length > keepRecent
      ? tools.slice(tools.length - keepRecent)
      : tools;

  return (
    <>
      {older.length > 0 && (
        <div className="tool-history-group">
          <button
            type="button"
            className="tool-history-toggle"
            onClick={() => setShowOlder((v) => !v)}
            aria-expanded={showOlder}
          >
            {showOlder ? "Hide" : "Show"} {older.length} earlier tool
            {older.length === 1 ? "" : "s"}
          </button>
          {showOlder &&
            older.map(({ item, index }) => (
              <ToolCallCard key={`tool-old-${index}`} item={item} />
            ))}
        </div>
      )}
      {recent.map(({ item, index }, i) => (
        <ToolCallCard
          key={`tool-r-${index}`}
          item={item}
          defaultOpen={
            i === recent.length - 1 &&
            (item.status === "running" || item.status === "pending")
          }
        />
      ))}
    </>
  );
}
