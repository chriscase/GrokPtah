/**
 * Pure helpers for progressive assistant stream application.
 *
 * Host may send:
 *  - cumulative full text (chunk starts with existing)
 *  - true deltas (append)
 *  - accidental full re-deliveries (multi-listener) → skip
 *
 * Never use length-only tail slicing for "what changed" when replacing
 * the committed bubble text — the bubble stores the full string; UI beam
 * components compute the visual delta themselves.
 */

export type StreamApplyResult =
  | { kind: "skip" }
  | { kind: "replace"; text: string }
  | { kind: "append"; text: string };

/**
 * Apply an incoming agent_message_chunk to the last assistant bubble text.
 * Returns the next full bubble text, or skip.
 */
export function applyAssistantStreamChunk(
  existing: string,
  chunk: string,
  opts?: { seq?: number; lastSeq?: number },
): StreamApplyResult {
  if (!chunk) return { kind: "skip" };

  // Monotonic seq: drop stale/out-of-order redeliveries when provided.
  if (
    opts?.seq != null &&
    opts.lastSeq != null &&
    opts.seq <= opts.lastSeq
  ) {
    return { kind: "skip" };
  }

  if (!existing) {
    return { kind: "replace", text: chunk };
  }

  // Cumulative snapshot (server/proxy re-sent a longer full buffer).
  if (chunk.startsWith(existing) && chunk.length > existing.length) {
    return { kind: "replace", text: chunk };
  }

  // Exact equality: long payload → treat as multi-listener full re-dump (skip).
  // Short payload may be a second identical code line (append) — #121.
  if (chunk === existing) {
    if (chunk.length >= 80) return { kind: "skip" };
    return { kind: "append", text: existing + chunk };
  }

  // Shorter rewind of a cumulative buffer we already have — ignore.
  // (Do NOT use endsWith(chunk): that drops legitimate repeated lines.)
  if (existing.startsWith(chunk) && existing.length > chunk.length) {
    return { kind: "skip" };
  }

  // True delta: append (including repeated identical lines).
  return { kind: "append", text: existing + chunk };
}

/**
 * Next bubble text after applying a chunk (convenience for callers).
 */
export function nextAssistantText(
  existing: string,
  chunk: string,
  opts?: { seq?: number; lastSeq?: number },
): string | "skip" {
  const r = applyAssistantStreamChunk(existing, chunk, opts);
  if (r.kind === "skip") return "skip";
  return r.text;
}

/**
 * Compute the *visual* newly-added suffix when text grows by content, not by
 * assuming length-only identity of the previous string.
 *
 * If `prev` is a prefix of `next`, return the suffix. If the stream rewound or
 * replaced, return full `next` (caller should reset beam segments).
 */
export function streamVisualDelta(prev: string, next: string): {
  reset: boolean;
  added: string;
} {
  if (next === prev) return { reset: false, added: "" };
  if (next.startsWith(prev)) {
    return { reset: false, added: next.slice(prev.length) };
  }
  // Find common prefix (content-based, not length-only identity).
  let i = 0;
  const n = Math.min(prev.length, next.length);
  while (i < n && prev.charCodeAt(i) === next.charCodeAt(i)) i++;
  // Mid-string edit / replace: reset beam to full next.
  if (i < prev.length) {
    return { reset: true, added: next };
  }
  return { reset: false, added: next.slice(i) };
}

/**
 * Stick-to-bottom: only auto-scroll when the user is already near the bottom.
 * `distanceFromBottom` = scrollHeight - scrollTop - clientHeight.
 */
export function shouldStickToBottom(
  distanceFromBottom: number,
  thresholdPx = 80,
): boolean {
  return distanceFromBottom <= thresholdPx;
}
