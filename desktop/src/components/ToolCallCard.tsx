import { memo, useEffect, useState } from "react";
import type { TranscriptItem } from "../lib/protocol";

type ToolItem = Extract<TranscriptItem, { kind: "tool" }>;

function statusLabel(status: unknown): string {
  if (typeof status === "string" && status.trim()) return status;
  if (status && typeof status === "object") {
    const keys = Object.keys(status as object);
    if (keys.length) return keys[0];
  }
  return "unknown";
}

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
      return "●";
    default:
      return "·";
  }
}

function shortOutput(output?: string, max = 88): string {
  if (!output) return "";
  const one = output.replace(/\s+/g, " ").trim();
  if (one.length <= max) return one;
  return `${one.slice(0, max)}…`;
}

/**
 * Compact tool row — collapsed by default.
 *
 * Only auto-expands while status is running/pending. On complete/fail it
 * collapses again unless the user has manually toggled this card.
 */
/** Memoized so settled tool cards skip re-render when other panes stream (#122). */
export const ToolCallCard = memo(function ToolCallCard({ item }: { item: ToolItem }) {
  const status = statusLabel(item.status);
  const live = status === "running" || status === "pending";
  /** null = follow automatic open rules; boolean = user override */
  const [userOpen, setUserOpen] = useState<boolean | null>(null);
  const title = item.title?.trim() || "tool";
  const preview = shortOutput(item.output);

  // New tool identity → clear any previous manual open state
  useEffect(() => {
    setUserOpen(null);
  }, [item.callId]);

  const open = userOpen !== null ? userOpen : live;

  return (
    <details
      className={`tool-card status-${status} ${live ? "is-live" : ""}`}
      open={open}
      onToggle={(e) => {
        // Capture user intent so we don't re-open finished tools after they click.
        setUserOpen((e.target as HTMLDetailsElement).open);
      }}
    >
      <summary className="tool-card-header">
        <span className="tool-card-glyph" aria-hidden>
          {statusGlyph(status)}
        </span>
        <span className="tool-card-title">{title}</span>
        <span className="tool-card-status">{status}</span>
        {!open && preview ? (
          <span className="tool-card-preview" title={item.output}>
            {preview}
          </span>
        ) : null}
        {!open && live && !preview ? (
          <span className="tool-card-preview">running…</span>
        ) : null}
      </summary>
      {item.output ? (
        <pre className="tool-card-output">{item.output}</pre>
      ) : live ? (
        <div className="tool-card-waiting">Running…</div>
      ) : (
        <div className="tool-card-waiting">(no output)</div>
      )}
    </details>
  );
});

/**
 * Show recent tools as compact rows; older ones stay behind a toggle.
 * Nothing is force-expanded when complete.
 */
export function ToolHistoryGroup({
  tools,
  keepRecent = 8,
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
    <div className="tool-history-group">
      {older.length > 0 && (
        <button
          type="button"
          className="tool-history-toggle"
          onClick={() => setShowOlder((v) => !v)}
          aria-expanded={showOlder}
        >
          {showOlder ? "Hide" : "Show"} {older.length} earlier tool
          {older.length === 1 ? "" : "s"}
        </button>
      )}
      {showOlder &&
        older.map(({ item, index }) => (
          <ToolCallCard key={`tool-old-${item.callId || index}`} item={item} />
        ))}
      {recent.map(({ item, index }) => (
        <ToolCallCard key={`tool-r-${item.callId || index}`} item={item} />
      ))}
    </div>
  );
}
