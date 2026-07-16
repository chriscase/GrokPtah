import { MarkdownBody } from "./MarkdownBody";
import { StreamingText } from "./StreamingText";
import { splitStreamingMarkdown } from "../lib/streamMarkdown";

/**
 * Progressive markdown while the model streams:
 * - **stable** prefix → full GFM render (headings, lists, tables, fences…)
 * - **tail** → Gemini-style beam-in tokens until that chunk stabilizes
 *
 * When `streaming` is false, renders the finished document as pure markdown
 * (with a brief settle class for polish).
 */
export function StreamingMarkdown({
  text,
  streaming,
}: {
  text: string;
  streaming?: boolean;
}) {
  if (!text) return null;

  if (!streaming) {
    return (
      <div className="stream-md stream-md-settled">
        <MarkdownBody text={text} />
      </div>
    );
  }

  const { stable, tail } = splitStreamingMarkdown(text);

  return (
    <div className="stream-md is-streaming">
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
        streaming && (
          <span className="stream-text is-streaming">
            <span className="stream-caret" aria-hidden />
          </span>
        )
      )}
    </div>
  );
}
