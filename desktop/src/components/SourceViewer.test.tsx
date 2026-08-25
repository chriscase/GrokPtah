import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { SourceViewer } from "./SourceViewer";
import type { SourceDocument } from "../lib/sourceView";

afterEach(cleanup);

function makeDocument(overrides: Partial<SourceDocument> = {}): SourceDocument {
  const lines = overrides.lines ?? [
    { number: 1, text: "fn main() {", truncated: false },
    { number: 2, text: '    let greeting = "hello";', truncated: false },
    { number: 3, text: "    println!(greeting);", truncated: false },
    { number: 4, text: "}", truncated: false },
  ];
  return {
    rootId: "ws-0123456789abcdef",
    rootKind: "workspace",
    rootPath: "/approved/repo/project",
    rootLabel: "repo/project",
    runId: null,
    relativePath: "src/main.rs",
    absolutePath: "/approved/repo/project/src/main.rs",
    language: "rust",
    encoding: "utf8",
    byteLen: 64,
    bytesRead: 64,
    lineCount: lines.length,
    truncatedBytes: false,
    truncatedLines: false,
    lossyReplacements: 0,
    eol: "lf",
    contentFingerprint: "fnv1a64:1111111111111111",
    ...overrides,
    lines,
  };
}

/** Many lines, so paging and reveal-on-search have something to do. */
function longDocument(total = 2000): SourceDocument {
  return makeDocument({
    lines: Array.from({ length: total }, (_, index) => ({
      number: index + 1,
      text: index === 999 ? "const needle = 1;" : `const filler${index} = 0;`,
      truncated: false,
    })),
    lineCount: total,
  });
}

describe("SourceViewer", () => {
  it("renders nothing when closed", () => {
    const { container } = render(
      <SourceViewer open={false} document={makeDocument()} onClose={vi.fn()} />,
    );
    expect(container).toBeEmptyDOMElement();
  });

  it("shows the file with real line numbers and its exact boundary", () => {
    render(<SourceViewer open document={makeDocument()} onClose={vi.fn()} />);

    expect(screen.getByRole("dialog", { name: "src/main.rs" })).toBeInTheDocument();
    expect(screen.getByTestId("source-viewer-identity")).toHaveTextContent(
      "Workspace · /approved/repo/project",
    );
    const first = screen.getByTestId("source-line-1");
    expect(first).toHaveTextContent("fn main() {");
    expect(within(first).getByText("1")).toBeInTheDocument();
    expect(screen.getByTestId("source-line-4")).toHaveTextContent("}");
  });

  it("names an isolated worktree and its run in the identity strip", () => {
    render(
      <SourceViewer
        open
        document={makeDocument({
          rootKind: "isolated_worktree",
          runId: "run-42",
          rootPath: "/approved/repo/project/.grokptah/worktrees/runs/run-42",
        })}
        onClose={vi.fn()}
      />,
    );
    expect(screen.getByTestId("source-viewer-identity")).toHaveTextContent(
      "Isolated worktree · run run-42 · /approved/repo/project/.grokptah/worktrees/runs/run-42",
    );
  });

  it("marks the code region as read only for assistive technology", () => {
    render(<SourceViewer open document={makeDocument()} onClose={vi.fn()} />);
    expect(screen.getByRole("group", { name: /src\/main\.rs, 4 lines, read only/ })).toBeInTheDocument();
  });

  it("hides decorative line numbers from the accessible name", () => {
    render(<SourceViewer open document={makeDocument()} onClose={vi.fn()} />);
    const number = within(screen.getByTestId("source-line-2")).getByText("2");
    expect(number).toHaveAttribute("aria-hidden", "true");
  });

  it("closes on Escape and restores focus to whatever opened it", async () => {
    const onClose = vi.fn();
    const opener = document.createElement("button");
    document.body.appendChild(opener);
    opener.focus();

    const { rerender } = render(
      <SourceViewer open document={makeDocument()} onClose={onClose} />,
    );
    expect(document.activeElement).not.toBe(opener);

    fireEvent.keyDown(window, { key: "Escape" });
    expect(onClose).toHaveBeenCalledOnce();

    rerender(<SourceViewer open={false} document={null} onClose={onClose} />);
    await waitFor(() => expect(document.activeElement).toBe(opener));
    opener.remove();
  });

  it("puts initial focus inside the dialog", () => {
    render(<SourceViewer open document={makeDocument()} onClose={vi.fn()} />);
    expect(document.activeElement).toBe(screen.getByTestId("source-viewer-code"));
  });

  it("keeps Tab inside the dialog", () => {
    render(<SourceViewer open document={makeDocument()} onClose={vi.fn()} />);
    // Same order the component's own focus trap walks, including the
    // tabbable code region.
    const focusable = Array.from(
      screen
        .getByTestId("source-viewer")
        .querySelectorAll<HTMLElement>(
          'button:not([disabled]), input:not([disabled]), [tabindex]:not([tabindex="-1"])',
        ),
    );
    const first = focusable[0];
    const last = focusable[focusable.length - 1];

    last.focus();
    fireEvent.keyDown(window, { key: "Tab" });
    expect(document.activeElement).toBe(first);

    fireEvent.keyDown(window, { key: "Tab", shiftKey: true });
    expect(document.activeElement).toBe(last);
  });

  it("moves focus to the search box on the find shortcut", () => {
    render(<SourceViewer open document={makeDocument()} onClose={vi.fn()} />);
    fireEvent.keyDown(window, { key: "f", metaKey: true });
    expect(document.activeElement).toBe(screen.getByTestId("source-viewer-search"));
  });

  it("counts in-file matches and announces the current one", () => {
    render(<SourceViewer open document={makeDocument()} onClose={vi.fn()} />);
    const search = screen.getByTestId("source-viewer-search");

    fireEvent.change(search, { target: { value: "greeting" } });
    expect(screen.getByTestId("source-viewer-match-count")).toHaveTextContent("1/2");

    fireEvent.keyDown(search, { key: "Enter" });
    expect(screen.getByTestId("source-viewer-live")).toHaveTextContent(
      "Match 1 of 2 for greeting",
    );

    fireEvent.keyDown(search, { key: "Enter" });
    expect(screen.getByTestId("source-viewer-live")).toHaveTextContent(
      "Match 2 of 2 for greeting",
    );
  });

  it("wraps backwards through matches", () => {
    render(<SourceViewer open document={makeDocument()} onClose={vi.fn()} />);
    const search = screen.getByTestId("source-viewer-search");
    fireEvent.change(search, { target: { value: "greeting" } });
    fireEvent.keyDown(search, { key: "Enter", shiftKey: true });
    expect(screen.getByTestId("source-viewer-live")).toHaveTextContent(
      "Match 2 of 2 for greeting",
    );
  });

  it("says so when a search finds nothing", () => {
    render(<SourceViewer open document={makeDocument()} onClose={vi.fn()} />);
    const search = screen.getByTestId("source-viewer-search");
    fireEvent.change(search, { target: { value: "absent" } });
    fireEvent.keyDown(search, { key: "Enter" });
    expect(screen.getByTestId("source-viewer-live")).toHaveTextContent("No matches for absent");
  });

  it("highlights matched text in place", () => {
    render(<SourceViewer open document={makeDocument()} onClose={vi.fn()} />);
    fireEvent.change(screen.getByTestId("source-viewer-search"), {
      target: { value: "greeting" },
    });
    const marks = screen.getAllByText("greeting");
    expect(marks.length).toBeGreaterThan(0);
    expect(marks[0].tagName).toBe("MARK");
  });

  it("opens at a requested line and announces where it landed", () => {
    render(
      <SourceViewer open document={longDocument()} initialLine={1000} onClose={vi.fn()} />,
    );
    expect(screen.getByTestId("source-line-1000")).toBeInTheDocument();
    expect(screen.queryByTestId("source-line-1")).not.toBeInTheDocument();
    expect(screen.getByTestId("source-viewer-live")).toHaveTextContent(
      "src/main.rs, line 1000 of 2000",
    );
  });

  it("pages long files with keyboard-reachable buttons in both directions", () => {
    render(
      <SourceViewer open document={longDocument()} initialLine={1000} onClose={vi.fn()} />,
    );
    expect(screen.queryByTestId("source-line-400")).not.toBeInTheDocument();

    fireEvent.click(screen.getByTestId("source-viewer-show-earlier"));
    expect(screen.getByTestId("source-line-600")).toBeInTheDocument();

    fireEvent.click(screen.getByTestId("source-viewer-show-more"));
    expect(screen.getByTestId("source-line-1500")).toBeInTheDocument();
  });

  it("brings an off-window match into view when stepping to it", () => {
    render(<SourceViewer open document={longDocument()} initialLine={1} onClose={vi.fn()} />);
    expect(screen.queryByTestId("source-line-1000")).not.toBeInTheDocument();

    const search = screen.getByTestId("source-viewer-search");
    fireEvent.change(search, { target: { value: "needle" } });
    fireEvent.keyDown(search, { key: "Enter" });

    expect(screen.getByTestId("source-line-1000")).toBeInTheDocument();
  });

  it("reports truncation instead of pretending the file is complete", () => {
    render(
      <SourceViewer
        open
        document={makeDocument({
          truncatedBytes: true,
          bytesRead: 64,
          byteLen: 9000,
          truncatedLines: true,
          lineCount: 5000,
        })}
        onClose={vi.fn()}
      />,
    );
    expect(screen.getByTestId("source-viewer-notice")).toHaveTextContent(
      "showing the first 64 of 9000 bytes",
    );
    expect(screen.getByTestId("source-viewer-notice")).toHaveTextContent(
      "showing the first 4 of 5000 lines",
    );
  });

  it("flags lossy decoding", () => {
    render(
      <SourceViewer
        open
        document={makeDocument({ encoding: "utf8_lossy", lossyReplacements: 2 })}
        onClose={vi.fn()}
      />,
    );
    expect(screen.getByTestId("source-viewer-notice")).toHaveTextContent(/not valid UTF-8/);
  });

  it("refuses to render a binary file as text", () => {
    render(
      <SourceViewer
        open
        document={makeDocument({ encoding: "binary", lines: [], lineCount: 0, byteLen: 2048 })}
        onClose={vi.fn()}
      />,
    );
    expect(screen.getByTestId("source-viewer-binary")).toHaveTextContent(
      "2048 bytes of binary content",
    );
    expect(screen.queryByTestId("source-viewer-code")).not.toBeInTheDocument();
    expect(screen.getByTestId("source-viewer-copy")).toBeDisabled();
  });

  it("explains a containment refusal in plain language and offers a retry", () => {
    const onRetry = vi.fn();
    render(
      <SourceViewer
        open
        document={null}
        error={new Error("symlink_rejected: `link` is a symbolic link")}
        onClose={vi.fn()}
        onRetry={onRetry}
      />,
    );
    const alert = screen.getByTestId("source-viewer-error");
    expect(alert).toHaveTextContent("crosses a symbolic link");
    fireEvent.click(within(alert).getByRole("button", { name: "Try again" }));
    expect(onRetry).toHaveBeenCalledOnce();
  });

  it("shows a loading state before any bytes arrive", () => {
    render(<SourceViewer open document={null} loading onClose={vi.fn()} />);
    expect(screen.getByText("Reading file…")).toBeInTheDocument();
  });

  it("copies the file text and says so", async () => {
    const onCopy = vi.fn().mockResolvedValue(undefined);
    render(<SourceViewer open document={makeDocument()} onClose={vi.fn()} onCopy={onCopy} />);

    fireEvent.click(screen.getByTestId("source-viewer-copy"));

    await waitFor(() =>
      expect(onCopy).toHaveBeenCalledWith(
        'fn main() {\n    let greeting = "hello";\n    println!(greeting);\n}',
      ),
    );
    await waitFor(() =>
      expect(screen.getByTestId("source-viewer-live")).toHaveTextContent(
        "Copied 4 lines of src/main.rs",
      ),
    );
  });

  it("reports a failed copy instead of silently doing nothing", async () => {
    const onCopy = vi.fn().mockRejectedValue(new Error("denied"));
    render(<SourceViewer open document={makeDocument()} onClose={vi.fn()} onCopy={onCopy} />);

    fireEvent.click(screen.getByTestId("source-viewer-copy"));

    await waitFor(() =>
      expect(screen.getByTestId("source-viewer-copy")).toHaveTextContent("Copy failed"),
    );
    expect(screen.getByTestId("source-viewer-live")).toHaveTextContent(/copy it manually/i);
  });

  it("marks a truncated line rather than hiding the cut", () => {
    render(
      <SourceViewer
        open
        document={makeDocument({
          lines: [{ number: 1, text: "x".repeat(16), truncated: true }],
          lineCount: 1,
        })}
        onClose={vi.fn()}
      />,
    );
    expect(screen.getByTestId("source-line-1")).toHaveTextContent("⋯");
  });

  it("resets search state when a different file is shown", () => {
    const { rerender } = render(
      <SourceViewer open document={makeDocument()} onClose={vi.fn()} />,
    );
    fireEvent.change(screen.getByTestId("source-viewer-search"), {
      target: { value: "greeting" },
    });
    fireEvent.keyDown(screen.getByTestId("source-viewer-search"), { key: "Enter" });

    rerender(
      <SourceViewer
        open
        document={makeDocument({
          relativePath: "src/other.rs",
          contentFingerprint: "fnv1a64:2222222222222222",
        })}
        onClose={vi.fn()}
      />,
    );
    expect(screen.getByTestId("source-viewer-live")).toHaveTextContent("src/other.rs, 4 lines");
  });
});
