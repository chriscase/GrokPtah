import { useEffect, useRef, useState } from "react";

/**
 * Gemini-style “beam in”: new tokens arrive with blur → clarity + soft glow,
 * not a plain dump of text. Fresh segments keep the animation class briefly
 * even after streaming ends so the last tokens still finish the effect.
 */
export function StreamingText({
  text,
  streaming,
}: {
  text: string;
  streaming?: boolean;
}) {
  const prevLen = useRef(0);
  const [segments, setSegments] = useState<
    { id: number; text: string; fresh: boolean }[]
  >(() => (text ? [{ id: 0, text, fresh: false }] : []));
  const idSeq = useRef(1);
  const settleTimers = useRef<number[]>([]);

  useEffect(() => {
    return () => {
      for (const t of settleTimers.current) window.clearTimeout(t);
    };
  }, []);

  useEffect(() => {
    if (text.length < prevLen.current) {
      prevLen.current = text.length;
      setSegments(text ? [{ id: idSeq.current++, text, fresh: false }] : []);
      return;
    }
    if (text.length === prevLen.current) return;

    const added = text.slice(prevLen.current);
    prevLen.current = text.length;
    if (!added) return;

    const id = idSeq.current++;
    setSegments((segs) => {
      // Keep a bounded number of segments so huge replies stay light.
      const settled = segs.map((s) => (s.fresh ? { ...s, fresh: false } : s));
      const next = [...settled, { id, text: added, fresh: true }];
      if (next.length > 80) {
        // Collapse oldest into one frozen segment
        const head = next
          .slice(0, next.length - 40)
          .map((s) => s.text)
          .join("");
        const tail = next.slice(next.length - 40);
        return [{ id: idSeq.current++, text: head, fresh: false }, ...tail];
      }
      return next;
    });

    // Clear “fresh” after animation completes so we don’t re-fire forever.
    const t = window.setTimeout(() => {
      setSegments((segs) =>
        segs.map((s) => (s.id === id ? { ...s, fresh: false } : s)),
      );
    }, 520);
    settleTimers.current.push(t);
  }, [text, streaming]);

  useEffect(() => {
    if (!streaming) {
      // Leave a short window so the last beam finishes.
      const t = window.setTimeout(() => {
        setSegments((segs) => segs.map((s) => ({ ...s, fresh: false })));
      }, 400);
      settleTimers.current.push(t);
    }
  }, [streaming]);

  if (!text) return null;

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
