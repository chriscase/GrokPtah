import { useEffect, useRef, useState } from "react";
import { tokenizeForMaterialize } from "../lib/materialize";

type Seg = { id: number; text: string; fresh: boolean };

/**
 * Gemini-style materialize: each *new* word is its own beam span.
 * Large deltas are subdivided so a 200-char SSE chunk still looks like
 * words arriving, not a full block popping in.
 */
export function StreamingText({
  text,
  streaming,
}: {
  text: string;
  streaming?: boolean;
}) {
  const prevLen = useRef(0);
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
    if (text.length < prevLen.current) {
      prevLen.current = 0;
      setSegments([]);
      for (const t of staggerTimers.current) window.clearTimeout(t);
      staggerTimers.current = [];
    }

    if (text.length === prevLen.current) return;

    const added = text.slice(prevLen.current);
    prevLen.current = text.length;
    if (!added) return;

    // Finished one-shot (full message at once): no multi-word beam spam
    if (!streaming && prevLen.current === added.length && segments.length === 0) {
      setSegments([{ id: idSeq.current++, text: added, fresh: false }]);
      return;
    }

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
  }, [text, streaming]);

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
