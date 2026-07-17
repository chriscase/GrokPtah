/**
 * Pure helpers for progressive assistant stream application.
 *
 * Host may send:
 *  - cumulative full text (chunk starts with existing and is longer) → replace
 *  - true deltas (append), including N identical lines in a row
 *  - optional monotonic `seq` to drop stale redeliveries
 *
 * Intentionally does **not**:
 *  - length-only tail slicing (see streamVisualDelta)
 *  - endsWith / startsWith "rewind" skips that drop the 3rd+ identical line
 *    when existing is "line\\nline\\n" and chunk is "line\\n" (#121)
 *  - length>=80 exact-equality skip that drops long repeated code lines
 *
 * Multi-listener full redelivery is handled by the session update bus
 * singleton; we prefer correct repeated-line append over heuristic skip.
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

  // Cumulative snapshot (server/proxy re-sent a longer full buffer that
  // already contains everything we have).
  if (chunk.startsWith(existing) && chunk.length > existing.length) {
    return { kind: "replace", text: chunk };
  }

  // Everything else is a delta: append.
  // Includes:
  //  - chunk === existing (2nd identical unit when bubble is just that unit)
  //  - existing.startsWith(chunk) (3rd+ identical line when bubble is N copies)
  //  - normal extensions that are not strict prefixes of the buffer
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
