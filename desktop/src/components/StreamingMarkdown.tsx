import { MarkdownBody } from "./MarkdownBody";
import { StreamingText } from "./StreamingText";
import { splitStreamingMarkdown } from "../lib/streamMarkdown";
import { useMaterializingText } from "../lib/useMaterializingText";

/**
 * Progressive markdown + Gemini-style materialization.
 *
 * 1. Large SSE chunks are **paced word-by-word** (`useMaterializingText`)
 * 2. Older content → full GFM (`stable`)
 * 3. Live tip (~100–280 chars) → beamed word spans (`tail`)
 * 4. On finish → full markdown with settle flash
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

  const { stable, tail } = splitStreamingMarkdown(visible);
  const catchingUp = pending > 0;

  return (
    <div
      className={`stream-md is-streaming ${catchingUp ? "is-catching-up" : ""}`}
    >
      {stable ? (
        <div className="stream-md-stable">
          <MarkdownBody text={stable} />
        </div>
      ) : null}
      {tail ? (
        <div className="stream-md-tail">
          <StreamingText text={tail} streaming />
        </div>
      ) : (
        <span className="stream-text is-streaming">
          <span className="stream-caret" aria-hidden />
        </span>
      )}
    </div>
  );
}
