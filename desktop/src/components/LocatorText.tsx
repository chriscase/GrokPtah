import { Fragment, memo, useMemo } from "react";
import { findSourceLocatorSpans, formatSourceLocator } from "../lib/sourceLocator";

export type LocatorTextProps = {
  text: string;
  /** Open a path directly. Without it the text renders plain. */
  onOpenSource?: (path: string, line: number | null) => void;
  /** Cap on how many links one block gets, so a huge log stays cheap. */
  limit?: number;
};

/**
 * Render text with file references turned into direct-open buttons.
 *
 * Tool output, test failures, and stack traces already name files and lines.
 * Making those names openable is the difference between reading a path and
 * reading the code it points at.
 *
 * Rendering is lossless: every character of `text` is emitted exactly once,
 * as text, so this cannot alter or inject content.
 */
export const LocatorText = memo(function LocatorText({
  text,
  onOpenSource,
  limit = 100,
}: LocatorTextProps) {
  const spans = useMemo(
    () => (onOpenSource ? findSourceLocatorSpans(text, limit) : []),
    [text, onOpenSource, limit],
  );

  if (!onOpenSource || spans.length === 0) return <>{text}</>;

  const parts: React.ReactNode[] = [];
  let cursor = 0;
  spans.forEach((span, index) => {
    if (span.start < cursor) return;
    if (span.start > cursor) {
      parts.push(<Fragment key={`t-${index}`}>{text.slice(cursor, span.start)}</Fragment>);
    }
    parts.push(
      <button
        key={`l-${index}`}
        type="button"
        className="locator-link"
        data-testid="locator-link"
        title={`Open ${formatSourceLocator(span.locator)} read-only`}
        aria-label={
          span.locator.line === null
            ? `Open ${span.locator.path}`
            : `Open ${span.locator.path} at line ${span.locator.line}`
        }
        onClick={() => onOpenSource(span.locator.path, span.locator.line)}
      >
        {span.text}
      </button>,
    );
    cursor = span.end;
  });
  if (cursor < text.length) parts.push(<Fragment key="t-end">{text.slice(cursor)}</Fragment>);

  return <>{parts}</>;
});
