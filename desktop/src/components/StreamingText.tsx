import { useEffect, useRef, useState } from "react";

/**
 * Gemini-style “beam in”: new tokens arrive with blur → clarity + soft glow.
 *
 * Important: do **not** seed `segments` with the full `text` and also run an
 * effect that appends `text.slice(prevLen)`. That double-painted one-shot
 * replies (chat invoke returns the full string at once) as "HelloHello".
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
  >([]);
  const idSeq = useRef(1);
  const settleTimers = useRef<number[]>([]);

  useEffect(() => {
    return () => {
      for (const t of settleTimers.current) window.clearTimeout(t);
    };
  }, []);

  useEffect(() => {
    // Shorter text = reset (rewind / replace with collapsed reply).
    if (text.length < prevLen.current) {
      prevLen.current = 0;
      setSegments([]);
    }

    if (text.length === prevLen.current) {
      return;
    }

    const added = text.slice(prevLen.current);
    prevLen.current = text.length;
    if (!added) return;

    const id = idSeq.current++;
    // One-shot full message (typical chat path): single settled segment, no beam spam.
    if (!streaming && prevLen.current === added.length) {
      setSegments([{ id, text: added, fresh: false }]);
      return;
    }

    setSegments((segs) => {
      const settled = segs.map((s) => (s.fresh ? { ...s, fresh: false } : s));
      const next = [...settled, { id, text: added, fresh: true }];
      if (next.length > 80) {
        const head = next
          .slice(0, next.length - 40)
          .map((s) => s.text)
          .join("");
        const tail = next.slice(next.length - 40);
        return [{ id: idSeq.current++, text: head, fresh: false }, ...tail];
      }
      return next;
    });

    // Hold the beam long enough to read the Gemini-style blur → clear settle.
    const t = window.setTimeout(() => {
      setSegments((segs) =>
        segs.map((s) => (s.id === id ? { ...s, fresh: false } : s)),
      );
    }, 720);
    settleTimers.current.push(t);
  }, [text, streaming]);

  useEffect(() => {
    if (!streaming) {
      const t = window.setTimeout(() => {
        setSegments((segs) => segs.map((s) => ({ ...s, fresh: false })));
      }, 480);
      settleTimers.current.push(t);
    }
  }, [streaming]);

  if (!text) return null;

  // Fallback if effect hasn't painted yet (first frame).
  if (segments.length === 0) {
    return (
      <span className={`stream-text ${streaming ? "is-streaming" : ""}`}>
        {text}
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
