import { MarkdownBody } from "./MarkdownBody";
import { StreamingText } from "./StreamingText";
import { useMaterializingText } from "../lib/useMaterializingText";

/**
 * Progressive markdown + paced live text.
 *
 * 1. Large SSE chunks are **paced word-by-word** (`useMaterializingText`)
 * 2. Live content stays in one stable text flow while chunks arrive
 * 3. On finish → full GFM markdown with settle flash
 */
export function StreamingMarkdown({
  text,
  streaming,
}: {
  text: string;
  streaming?: boolean;
}) {
  const live = !!streaming;
  const { visible, pending } = useMaterializingText(text, live);

  if (!text) return null;

  if (!live) {
    return (
      <div className="stream-md stream-md-settled">
        <MarkdownBody text={text} />
      </div>
    );
  }

  const catchingUp = pending > 0;

  return (
    <div
      className={`stream-md is-streaming ${catchingUp ? "is-catching-up" : ""}`}
    >
      {/* Keep the live buffer in one stable text flow. Parsing incomplete
       * markdown while chunks arrive can repeatedly change block geometry and
       * briefly collapse the newest text against the left edge. Full markdown
       * rendering resumes once the turn settles. */}
      <div className="stream-md-live">
        {visible ? (
          <StreamingText text={visible} streaming animated={false} />
        ) : (
          <span className="stream-text is-streaming">
            <span className="stream-caret" aria-hidden />
          </span>
        )}
      </div>
    </div>
  );
}
