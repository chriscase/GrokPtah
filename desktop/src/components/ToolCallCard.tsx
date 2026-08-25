import { memo, useEffect, useId, useRef, useState } from "react";
import type { TranscriptItem } from "../lib/protocol";
import "./ToolCallCard.css";

type ToolItem = Extract<TranscriptItem, { kind: "tool" }>;

export type ToolStatusKind =
  | "completed"
  | "failed"
  | "denied"
  | "running"
  | "pending"
  | "unknown";

const KNOWN_STATUSES: readonly string[] = [
  "completed",
  "failed",
  "denied",
  "running",
  "pending",
];

/**
 * Restored transcripts and older bridges can carry statuses outside the
 * ToolCallStatus union (arbitrary strings, or Rust-enum objects like
 * `{ failed: {...} }`). Collapse everything to a bounded, class-safe kind and
 * keep the raw text so the expanded view can still show what was reported.
 */
export function normalizeToolStatus(status: unknown): {
  kind: ToolStatusKind;
  raw: string;
} {
  let raw = "";
  if (typeof status === "string") {
    raw = status.trim();
  } else if (status && typeof status === "object") {
    const keys = Object.keys(status as object);
    if (keys.length) raw = keys[0].trim();
  }
  const lower = raw.toLowerCase();
  const kind = KNOWN_STATUSES.includes(lower)
    ? (lower as ToolStatusKind)
    : "unknown";
  return { kind, raw };
}

function statusGlyph(kind: ToolStatusKind): string {
  switch (kind) {
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
      return "?";
  }
}

function shortOutput(output?: string, max = 88): string {
  if (!output) return "";
  const one = output.replace(/\s+/g, " ").trim();
  if (one.length <= max) return one;
  return `${one.slice(0, max)}…`;
}

const COPY_RESET_MS = 2000;
/** Bound how much of an odd raw status we ever render. */
const RAW_STATUS_MAX = 40;

/**
 * Compact tool row — collapsed by default.
 *
 * Only auto-expands while status is running/pending. On complete/fail it
 * collapses again unless the user has manually toggled this card.
 */
/** Memoized so settled tool cards skip re-render when other panes stream (#122). */
export const ToolCallCard = memo(function ToolCallCard({
  item,
}: {
  item: ToolItem;
}) {
  const { kind, raw } = normalizeToolStatus(item.status);
  const live = kind === "running" || kind === "pending";
  /** null = follow automatic open rules; boolean = user override */
  const [userOpen, setUserOpen] = useState<boolean | null>(null);
  const [copyState, setCopyState] = useState<"idle" | "copied" | "failed">(
    "idle",
  );
  const copyTimer = useRef<number | null>(null);
  const bodyId = useId();
  const title = item.title?.trim() || "tool";
  const preview = shortOutput(item.output);

  // New tool identity → clear any previous manual open state
  useEffect(() => {
    setUserOpen(null);
  }, [item.callId]);

  useEffect(
    () => () => {
      if (copyTimer.current !== null) window.clearTimeout(copyTimer.current);
    },
    [],
  );

  const open = userOpen !== null ? userOpen : live;
  const canCopy =
    Boolean(item.output) &&
    typeof navigator !== "undefined" &&
    typeof navigator.clipboard?.writeText === "function";

  // Restrained announcements: only terminal states the user must not miss.
  // Text depends solely on (title, kind), so streaming output never re-fires it.
  const announcement =
    kind === "failed"
      ? `${title} failed`
      : kind === "denied"
        ? `${title} denied`
        : "";

  const oddRawStatus =
    kind === "unknown" && raw && raw.toLowerCase() !== "unknown"
      ? raw.slice(0, RAW_STATUS_MAX)
      : "";

  const settleCopy = (state: "copied" | "failed") => {
    setCopyState(state);
    if (copyTimer.current !== null) window.clearTimeout(copyTimer.current);
    copyTimer.current = window.setTimeout(
      () => setCopyState("idle"),
      COPY_RESET_MS,
    );
  };

  const handleCopy = () => {
    const text = item.output ?? "";
    navigator.clipboard.writeText(text).then(
      () => settleCopy("copied"),
      // Clipboard refused (permissions/webview quirk) — a silent no-op reads
      // as success and ships stale evidence; say it failed.
      () => settleCopy("failed"),
    );
  };

  return (
    <div
      className={`tool-card status-${kind}${live ? " is-live" : ""}`}
      data-status={kind}
    >
      <button
        type="button"
        className="tool-card-header"
        aria-expanded={open}
        aria-controls={bodyId}
        onClick={() => setUserOpen(!open)}
      >
        <span className="tool-card-glyph" aria-hidden="true">
          {statusGlyph(kind)}
        </span>
        <span className="tool-card-title" title={title}>
          {title}
        </span>
        <span className="tool-card-status">{kind}</span>
        {!open && preview ? (
          <span className="tool-card-preview" aria-hidden="true">
            {preview}
          </span>
        ) : null}
        {!open && live && !preview ? (
          <span className="tool-card-preview" aria-hidden="true">
            {kind === "pending" ? "waiting…" : "running…"}
          </span>
        ) : null}
        <span className="tool-card-caret" aria-hidden="true">
          {open ? "▾" : "▸"}
        </span>
      </button>
      <div id={bodyId} className="tool-card-body" hidden={!open}>
        {!open ? null : item.output ? (
          <>
            {oddRawStatus || canCopy ? (
              <div className="tool-card-meta">
                {oddRawStatus ? (
                  <span className="tool-card-meta-status">
                    reported status: {oddRawStatus}
                  </span>
                ) : null}
                {canCopy ? (
                  <>
                    <button
                      type="button"
                      className={`tool-card-copy${copyState === "failed" ? " is-failed" : ""}`}
                      onClick={handleCopy}
                    >
                      {copyState === "copied"
                        ? "Copied"
                        : copyState === "failed"
                          ? "Copy failed"
                          : "Copy output"}
                    </button>
                    <span
                      className="sr-only"
                      role="status"
                      data-testid="tool-card-copy-status"
                    >
                      {copyState === "copied"
                        ? "Output copied"
                        : copyState === "failed"
                          ? "Copy failed"
                          : ""}
                    </span>
                  </>
                ) : null}
              </div>
            ) : null}
            <pre className="tool-card-output">{item.output}</pre>
          </>
        ) : live ? (
          <div className="tool-card-waiting">
            {kind === "pending" ? "Waiting to start…" : "Running…"}
          </div>
        ) : (
          <div className="tool-card-waiting">
            {oddRawStatus ? `(no output — reported status: ${oddRawStatus})` : "(no output)"}
          </div>
        )}
      </div>
      <span
        className="sr-only"
        role="status"
        data-testid="tool-card-announce"
      >
        {announcement}
      </span>
    </div>
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
  const olderId = useId();
  if (tools.length === 0) return null;

  const older =
    tools.length > keepRecent ? tools.slice(0, tools.length - keepRecent) : [];
  const recent =
    tools.length > keepRecent
      ? tools.slice(tools.length - keepRecent)
      : tools;

  return (
    <div
      className="tool-history-group"
      role="group"
      aria-label={`Tool activity: ${tools.length} call${tools.length === 1 ? "" : "s"}`}
    >
      {older.length > 0 && (
        <button
          type="button"
          className="tool-history-toggle"
          onClick={() => setShowOlder((v) => !v)}
          aria-expanded={showOlder}
          aria-controls={olderId}
        >
          {showOlder ? "Hide" : "Show"} {older.length} earlier tool
          {older.length === 1 ? "" : "s"}
        </button>
      )}
      {older.length > 0 && (
        <div
          id={olderId}
          className="tool-history-older"
          hidden={!showOlder}
        >
          {showOlder
            ? older.map(({ item, index }) => (
                <ToolCallCard
                  key={`tool-old-${item.callId || index}`}
                  item={item}
                />
              ))
            : null}
        </div>
      )}
      {recent.map(({ item, index }) => (
        <ToolCallCard key={`tool-r-${item.callId || index}`} item={item} />
      ))}
    </div>
  );
}
