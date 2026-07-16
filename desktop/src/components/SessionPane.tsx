import { useEffect, useMemo, useRef } from "react";
import type { SessionTab, TranscriptItem } from "../lib/protocol";
import {
  expandDebugLines,
  isDebugThought,
} from "../lib/debugTrace";
import { ActivityIndicator } from "./ActivityIndicator";
import { DebugTrace } from "./DebugTrace";
import { MarkdownBody } from "./MarkdownBody";
import { StreamingText } from "./StreamingText";
import { ToolCallCard, ToolHistoryGroup } from "./ToolCallCard";
import { api } from "../lib/api";

type ToolItem = Extract<TranscriptItem, { kind: "tool" }>;

type RenderRow =
  | { type: "item"; item: TranscriptItem; index: number }
  | { type: "debug"; key: string; lines: string[]; live: boolean }
  | {
      type: "tool_batch";
      key: string;
      tools: { item: ToolItem; index: number }[];
    };

/**
 * Group consecutive tool calls into batches so older ones collapse together,
 * and fold host debug thoughts into chips.
 */
function groupTranscript(items: TranscriptItem[]): RenderRow[] {
  const out: RenderRow[] = [];
  let i = 0;
  while (i < items.length) {
    const item = items[i];
    if (item.kind === "thought" && isDebugThought(item.text)) {
      const start = i;
      const lines: string[] = [];
      let live = false;
      while (i < items.length) {
        const cur = items[i];
        if (cur.kind !== "thought" || !isDebugThought(cur.text)) break;
        if (cur.streaming) live = true;
        for (const line of expandDebugLines(cur.text)) {
          if (!lines.includes(line)) lines.push(line);
        }
        i += 1;
      }
      out.push({ type: "debug", key: `dbg-${start}`, lines, live });
      continue;
    }
    if (item.kind === "tool") {
      const tools: { item: ToolItem; index: number }[] = [];
      while (i < items.length && items[i].kind === "tool") {
        tools.push({
          item: items[i] as ToolItem,
          index: i,
        });
        i += 1;
      }
      out.push({
        type: "tool_batch",
        key: `tools-${tools[0]?.index ?? 0}`,
        tools,
      });
      continue;
    }
    out.push({ type: "item", item, index: i });
    i += 1;
  }
  return out;
}

export type SessionPaneProps = {
  tab: SessionTab;
  focused: boolean;
  kindLabel?: string;
  emptyHint?: string;
  bridgeVersion?: string;
  onFocus: () => void;
  onClosePane?: () => void;
  /** Show close control for secondary pane only. */
  showClose?: boolean;
};

/**
 * One session column: header, transcript, activity.
 * Composer stays shared in the parent and targets the focused pane.
 */
export function SessionPane({
  tab,
  focused,
  kindLabel,
  emptyHint,
  bridgeVersion,
  onFocus,
  onClosePane,
  showClose,
}: SessionPaneProps) {
  const bottomRef = useRef<HTMLDivElement>(null);
  const busy = tab.busy;
  const transcript = tab.transcript;
  const rows = useMemo(() => groupTranscript(transcript), [transcript]);

  useEffect(() => {
    if (focused) {
      bottomRef.current?.scrollIntoView({ behavior: "smooth" });
    }
  }, [transcript, focused, tab.id]);

  return (
    <section
      className={`session-pane ${focused ? "is-focused" : ""} ${busy ? "is-busy" : ""}`}
      onMouseDown={onFocus}
      data-session-id={tab.id}
    >
      <header className="session-pane-header">
        <div className="session-pane-title">
          {tab.needsPermission ? (
            <span className="attn-dot permission" title="Needs response" />
          ) : tab.busy ? (
            <span className="busy-dot" title="Working" />
          ) : tab.unseen ? (
            <span className="attn-dot unseen" title="Unseen" />
          ) : null}
          {kindLabel && (
            <span className={`kind-chip ${kindLabel}`}>{kindLabel}</span>
          )}
          <span className="session-pane-name" title={tab.title}>
            {tab.title}
          </span>
        </div>
        <div className="session-pane-actions">
          {focused && <span className="session-pane-focus-tag">focused</span>}
          {showClose && onClosePane && (
            <button
              type="button"
              className="session-pane-close"
              title="Close side pane"
              aria-label="Close side pane"
              onClick={(e) => {
                e.stopPropagation();
                onClosePane();
              }}
            >
              ×
            </button>
          )}
        </div>
      </header>

      <div className="transcript session-pane-transcript">
        {transcript.length === 0 && (
          <div className="empty-agent pane-empty">
            <h1>{tab.title || "Session"}</h1>
            {bridgeVersion && (
              <div className="version-line">bridge {bridgeVersion}</div>
            )}
            <p className="pane-empty-hint">
              {emptyHint ?? "Send a message when this pane is focused."}
            </p>
          </div>
        )}
        {rows.map((row) => {
          if (row.type === "debug") {
            return (
              <DebugTrace
                key={row.key}
                lines={row.lines}
                live={busy && row.live}
              />
            );
          }
          if (row.type === "tool_batch") {
            return (
              <div key={row.key} className="tool-batch">
                <ToolHistoryGroup tools={row.tools} keepRecent={4} />
              </div>
            );
          }
          const { item, index: i } = row;
          return (
            <div key={i} className={`bubble ${item.kind}`}>
              {item.kind === "tool" && <ToolCallCard item={item} />}
              {item.kind === "plan" && (
                <>
                  <strong>Plan ({item.status})</strong>
                  <ol>
                    {item.steps.map((s, j) => (
                      <li key={j}>{s}</li>
                    ))}
                  </ol>
                  {focused && item.status === "proposed" && (
                    <div className="modal-actions">
                      <button
                        type="button"
                        className="primary"
                        onClick={() => {
                          void api.acceptPlan(tab.id).catch((e) => {
                            console.warn("acceptPlan failed", e);
                          });
                        }}
                      >
                        Accept & execute
                      </button>
                      <button
                        type="button"
                        onClick={() => void api.rejectPlan(tab.id)}
                      >
                        Reject
                      </button>
                    </div>
                  )}
                </>
              )}
              {item.kind === "assistant" &&
                (item.streaming ? (
                  <StreamingText text={item.text} streaming />
                ) : (
                  <MarkdownBody text={item.text} />
                ))}
              {item.kind === "thought" && (
                <StreamingText text={item.text} streaming={item.streaming} />
              )}
              {item.kind === "user" && (
                <div className="user-text">{item.text}</div>
              )}
              {item.kind === "error" && (
                <div className="error-text">{item.text}</div>
              )}
            </div>
          );
        })}
        <div ref={bottomRef} />
      </div>

      <ActivityIndicator activity={tab.activity} busy={busy} />
    </section>
  );
}
