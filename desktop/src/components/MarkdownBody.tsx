import { memo } from "react";
import ReactMarkdown, { type Components } from "react-markdown";
import remarkGfm from "remark-gfm";

/**
 * Models often emit GFM tables on one line:
 * `| A | B | |---| | c | d |`
 * remark-gfm needs real newlines between rows. Also empty mid-row cells
 * look like `| |` — only split when the next segment looks like a new row
 * (separator or another full pipe-row), not a single empty cell.
 */
export function normalizeMarkdownTables(text: string): string {
  if (!text.includes("|")) return text;

  const parts = text.split(/(```[\s\S]*?```)/g);
  return parts
    .map((part) => {
      if (part.startsWith("```")) return part;
      return part
        .replace(/\r\n/g, "\n")
        // Before a separator row: `| ... | |---|`
        .replace(/\|\s+\|(?=\s*:?-+:?)/g, "|\n|")
        // After separator, before next data row: `|---| | Data |`
        .replace(/(\|[\t \-:|]+\|)\s+\|/g, "$1\n|")
        // Glued data rows: `| a | b | | c | d |` → newline between rows.
        .replace(/\|\s+\|(?=[^|\n]*\|[^|\n]*\|)/g, "|\n|");
    })
    .join("");
}

/** Stable markdown component map — recreated each render breaks React.memo. */
const MD_COMPONENTS: Components = {
  a: ({ href, children }) => (
    <a href={href} target="_blank" rel="noreferrer noopener">
      {children}
    </a>
  ),
  table: ({ children }) => (
    <div className="md-table-wrap">
      <table>{children}</table>
    </div>
  ),
  th: ({ children, style }) => (
    <th style={style} scope="col">
      {children}
    </th>
  ),
  td: ({ children, style }) => <td style={style}>{children}</td>,
  pre: ({ children }) => <pre className="md-pre">{children}</pre>,
  code: ({ className, children, ...props }) => {
    const inline = !className;
    if (inline) {
      return (
        <code className="md-code-inline" {...props}>
          {children}
        </code>
      );
    }
    return (
      <code className={className} {...props}>
        {children}
      </code>
    );
  },
};

/**
 * Render assistant (and similar) text as markdown — not raw source.
 * Streaming callers should pass plain text until the turn settles.
 * Memoized so settled bubbles skip re-parse during unrelated stream chunks (#122).
 */
export const MarkdownBody = memo(function MarkdownBody({ text }: { text: string }) {
  if (!text) return null;
  const source = normalizeMarkdownTables(text);
  return (
    <div className="md-body">
      <ReactMarkdown remarkPlugins={[remarkGfm]} components={MD_COMPONENTS}>
        {source}
      </ReactMarkdown>
    </div>
  );
});
