import { useEffect, useId, useRef, useState } from "react";
import { debugChipLabel } from "../lib/debugTrace";

export type DebugTraceProps = {
  lines: string[];
  /** When true, chip shows a soft live pulse (turn still running). */
  live?: boolean;
};

/**
 * Collapses host diagnostics (kind/model/auth, API status) into a quiet chip.
 * Click opens a compact panel — keeps the transcript readable.
 */
export function DebugTrace({ lines, live = false }: DebugTraceProps) {
  const [open, setOpen] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);
  const panelId = useId();

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    const onDown = (e: MouseEvent) => {
      if (rootRef.current && !rootRef.current.contains(e.target as Node)) {
        setOpen(false);
      }
    };
    window.addEventListener("keydown", onKey);
    window.addEventListener("mousedown", onDown, true);
    return () => {
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("mousedown", onDown, true);
    };
  }, [open]);

  if (lines.length === 0) return null;

  const label = debugChipLabel(lines);
  const count = lines.length;

  return (
    <div
      ref={rootRef}
      className={`debug-trace ${open ? "is-open" : ""} ${live ? "is-live" : ""}`}
    >
      <button
        type="button"
        className="debug-trace-chip"
        aria-expanded={open}
        aria-controls={panelId}
        title="Show turn diagnostics"
        onClick={() => setOpen((v) => !v)}
      >
        <span className="debug-trace-glyph" aria-hidden>
          <svg width="12" height="12" viewBox="0 0 12 12" fill="none">
            <circle
              cx="6"
              cy="6"
              r="4.25"
              stroke="currentColor"
              strokeWidth="1.2"
            />
            <path
              d="M6 5.2v3.1M6 3.6h.01"
              stroke="currentColor"
              strokeWidth="1.3"
              strokeLinecap="round"
            />
          </svg>
        </span>
        <span className="debug-trace-label">{label}</span>
        {count > 1 && <span className="debug-trace-count">{count}</span>}
        <span className="debug-trace-caret" aria-hidden>
          {open ? "▾" : "▸"}
        </span>
      </button>

      {open && (
        <div id={panelId} className="debug-trace-panel" role="region">
          <div className="debug-trace-panel-head">
            <span>Diagnostics</span>
            <button
              type="button"
              className="debug-trace-copy"
              onClick={async () => {
                try {
                  await navigator.clipboard.writeText(lines.join("\n"));
                } catch {
                  /* ignore */
                }
              }}
            >
              Copy
            </button>
          </div>
          <ul className="debug-trace-list">
            {lines.map((line, i) => (
              <li key={i}>
                <code>{line}</code>
              </li>
            ))}
          </ul>
        </div>
      )}
    </div>
  );
}
