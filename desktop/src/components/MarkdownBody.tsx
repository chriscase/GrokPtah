import ReactMarkdown from "react-markdown";

/**
 * Render assistant (and similar) text as markdown — not raw source.
 * Streaming callers should pass plain text until the turn settles.
 */
export function MarkdownBody({ text }: { text: string }) {
  if (!text) return null;
  return (
    <div className="md-body">
      <ReactMarkdown
        components={{
          a: ({ href, children }) => (
            <a href={href} target="_blank" rel="noreferrer noopener">
              {children}
            </a>
          ),
          // Avoid oversized pre blocks in the transcript
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
        }}
      >
        {text}
      </ReactMarkdown>
    </div>
  );
}
