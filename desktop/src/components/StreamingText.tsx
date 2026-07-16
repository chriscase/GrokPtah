import { useEffect, useRef, useState } from "react";

/**
 * Gemini-style stream reveal: each newly arrived suffix fades/blurs in so
 * tokens feel painted rather than dumped as a block.
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

  useEffect(() => {
    if (text.length < prevLen.current) {
      // Rewind / replace whole message
      prevLen.current = text.length;
      setSegments(text ? [{ id: idSeq.current++, text, fresh: false }] : []);
      return;
    }
    if (text.length === prevLen.current) return;

    const added = text.slice(prevLen.current);
    prevLen.current = text.length;
    if (!added) return;

    setSegments((segs) => {
      const settled = segs.map((s) =>
        s.fresh ? { ...s, fresh: false } : s,
      );
      return [
        ...settled,
        {
          id: idSeq.current++,
          text: added,
          fresh: !!streaming,
        },
      ];
    });
  }, [text, streaming]);

  // When streaming ends, drop fresh flags so we don't re-animate on remount.
  useEffect(() => {
    if (!streaming) {
      setSegments((segs) => segs.map((s) => ({ ...s, fresh: false })));
    }
  }, [streaming]);

  if (!text) return null;

  return (
    <span className="stream-text">
      {segments.map((seg) => (
        <span
          key={seg.id}
          className={seg.fresh ? "stream-token stream-token-fresh" : "stream-token"}
        >
          {seg.text}
        </span>
      ))}
    </span>
  );
}
