import { useEffect, useRef, useState } from "react";
import { tokenizeForMaterialize } from "../lib/materialize";
import { streamVisualDelta } from "../lib/streamApply";

type Seg = { id: number; text: string; fresh: boolean };

/**
 * Gemini-style materialize: each *new* word is its own beam span.
 * Large deltas are subdivided so a 200-char SSE chunk still looks like
 * words arriving, not a full block popping in.
 *
 * Visual delta is content-based (`streamVisualDelta`), not length-only
 * `text.slice(prevLen)` — length-only garbles when the buffer is rewritten
 * with the same length or a non-prefix replacement.
 */
export function StreamingText({
  text,
  streaming,
}: {
  text: string;
  streaming?: boolean;
}) {
  const prevText = useRef("");
  const [segments, setSegments] = useState<Seg[]>([]);
  const idSeq = useRef(1);
  const settleTimers = useRef<number[]>([]);
  const staggerTimers = useRef<number[]>([]);

  useEffect(() => {
    return () => {
      for (const t of settleTimers.current) window.clearTimeout(t);
      for (const t of staggerTimers.current) window.clearTimeout(t);
    };
  }, []);

  useEffect(() => {
    const { reset, added } = streamVisualDelta(prevText.current, text);
    if (reset) {
      prevText.current = "";
      setSegments([]);
      for (const t of staggerTimers.current) window.clearTimeout(t);
      staggerTimers.current = [];
      // After reset, treat full text as the addition
      const full = streamVisualDelta("", text);
      prevText.current = text;
      if (!full.added) return;
      ingestAdded(full.added, streaming, segments.length === 0);
      return;
    }

    if (!added) return;
    prevText.current = text;

    // Finished one-shot (full message at once): no multi-word beam spam
    if (!streaming && prevText.current === added && segments.length === 0) {
      setSegments([{ id: idSeq.current++, text: added, fresh: false }]);
      return;
    }

    ingestAdded(added, streaming, false);
  }, [text, streaming]);

  function ingestAdded(
    added: string,
    streaming: boolean | undefined,
    _emptySegs: boolean,
  ) {
    const tokens = tokenizeForMaterialize(added);
    // Tiny delta — one beam unit
    if (tokens.length <= 1) {
      pushSegment(added);
      return;
    }

    // Large delta: stagger word beams so it materializes
    const stagger = streaming ? 18 : 0;
    tokens.forEach((tok, i) => {
      if (stagger === 0) {
        pushSegment(tok);
        return;
      }
      const t = window.setTimeout(() => pushSegment(tok), i * stagger);
      staggerTimers.current.push(t);
    });
  }

  function pushSegment(piece: string) {
    if (!piece) return;
    const id = idSeq.current++;
    setSegments((segs) => {
      const settled = segs.map((s) => (s.fresh ? { ...s, fresh: false } : s));
      const next = [...settled, { id, text: piece, fresh: true }];
      // Cap segment list for long streams
      if (next.length > 120) {
        const head = next
          .slice(0, next.length - 50)
          .map((s) => s.text)
          .join("");
        const tail = next.slice(next.length - 50);
        return [{ id: idSeq.current++, text: head, fresh: false }, ...tail];
      }
      return next;
    });
    const t = window.setTimeout(() => {
      setSegments((segs) =>
        segs.map((s) => (s.id === id ? { ...s, fresh: false } : s)),
      );
    }, 850);
    settleTimers.current.push(t);
  }

  useEffect(() => {
    if (!streaming) {
      const t = window.setTimeout(() => {
        setSegments((segs) => segs.map((s) => ({ ...s, fresh: false })));
      }, 200);
      settleTimers.current.push(t);
    }
  }, [streaming]);

  if (!text) return null;

  if (segments.length === 0) {
    return (
      <span className={`stream-text ${streaming ? "is-streaming" : ""}`}>
        <span className={streaming ? "stream-token stream-token-beam" : "stream-token"}>
          {text}
        </span>
        {streaming && <span className="stream-caret" aria-hidden />}
      </span>
    );
  }

  return (
    <span className={`stream-text ${streaming ? "is-streaming" : ""}`}>
      {segments.map((seg) => (
        <span
          key={seg.id}
          className={
            seg.fresh ? "stream-token stream-token-beam" : "stream-token"
          }
        >
          {seg.text}
        </span>
      ))}
      {streaming && <span className="stream-caret" aria-hidden />}
    </span>
  );
}
