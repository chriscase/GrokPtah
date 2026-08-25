import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { SourceViewer } from "./SourceViewer";
import { SOURCE_VIEW_CONTRACT, type SourceDocument, type SourceLine } from "../lib/sourceView";

afterEach(cleanup);

const SNAP = "0123456789abcdef0123456789abcdef";
const DIGEST_A = `${"0".repeat(63)}1`;
const DIGEST_B = `${"a".repeat(63)}b`;
const DIGEST_C = `${"c".repeat(63)}d`;
const TOKEN = `sv1.${SNAP}.0.00112233445566778899aabbccddeeff`;

const WORKSPACE_ROOT = {
  token: TOKEN,
  kind: "workspace" as const,
  label: "repo/project",
  pathDigest: DIGEST_A,
  identityDigest: DIGEST_B,
  runId: null,
};

function lines(...texts: string[]): SourceLine[] {
  return texts.map((text, index) => ({ number: index + 1, text, truncated: false }));
}

const BODY = lines(
  "fn main() {",
  '    let greeting = "hello";',
  "    println!(greeting);",
  "}",
);

function makeDocument(overrides: Partial<SourceDocument> = {}): SourceDocument {
  return {
    contract: SOURCE_VIEW_CONTRACT,
    root: WORKSPACE_ROOT,
    snapshotId: SNAP,
    revision: 1,
    relativePath: "src/main.rs",
    language: "rust",
    byteLen: 64,
    content: { verdict: "text", scannedBytes: 64, completeScan: true },
    identity: { kind: "content", digest: DIGEST_A },
    limits: { maxBytes: 524_288, maxLines: 1_200, maxLineChars: 2_000 },
    chunk: {
      lines: BODY,
      startByte: 0,
      bytesConsumed: 64,
      lossyReplacements: 0,
      eol: "lf",
      continuesPrevious: false,
      continuesNext: false,
      nextCursor: null,
      eof: true,
    },
    ...overrides,
  };
}

/** A sibling element standing in for the application behind the modal. */
function withBackground(): HTMLElement {
  const background = document.createElement("div");
  background.id = "app-background";
  background.innerHTML = "<button id='background-button'>behind</button>";
  document.body.appendChild(background);
  return background;
}

beforeEach(() => {
  // jsdom has no layout, so scrollIntoView must be stubbed for reveal paths.
  Element.prototype.scrollIntoView = vi.fn();
});

describe("SourceViewer", () => {
  it("renders nothing when closed", () => {
    const { container } = render(
      <SourceViewer open={false} document={makeDocument()} lines={BODY} onClose={vi.fn()} />,
    );
    expect(container).toBeEmptyDOMElement();
  });

  it("shows the file with real line numbers and a path-free identity", () => {
    render(<SourceViewer open document={makeDocument()} lines={BODY} onClose={vi.fn()} />);

    expect(screen.getByRole("dialog", { name: "src/main.rs" })).toBeInTheDocument();
    const identity = screen.getByTestId("source-viewer-identity");
    expect(identity).toHaveTextContent("Workspace · repo/project · 000000000000");
    // The label is the last two path segments; what must never appear is the
    // host's absolute location.
    expect(identity.textContent?.startsWith("/")).toBe(false);
    expect(identity.textContent).not.toMatch(/(^|\s)\/[A-Za-z]/);
    const first = screen.getByTestId("source-line-1");
    expect(first).toHaveTextContent("fn main() {");
    expect(within(first).getByText("1")).toHaveAttribute("aria-hidden", "true");
  });

  it("names an isolated worktree and its run without naming a path", () => {
    render(
      <SourceViewer
        open
        lines={BODY}
        document={makeDocument({
          root: {
            ...WORKSPACE_ROOT,
            kind: "isolated_worktree",
            runId: "run-42",
            label: "runs/run-42",
            pathDigest: DIGEST_C,
          },
        })}
        onClose={vi.fn()}
      />,
    );
    expect(screen.getByTestId("source-viewer-identity")).toHaveTextContent(
      "Isolated worktree · run run-42 · runs/run-42 · cccccccccccc",
    );
  });

  // --- accessibility -----------------------------------------------------

  it("makes the rest of the page inert while it is open and restores it after", () => {
    const background = withBackground();
    const { rerender } = render(
      <SourceViewer open document={makeDocument()} lines={BODY} onClose={vi.fn()} />,
    );
    expect(background).toHaveAttribute("inert");
    expect(background).toHaveAttribute("aria-hidden", "true");

    rerender(<SourceViewer open={false} document={null} lines={[]} onClose={vi.fn()} />);
    expect(background).not.toHaveAttribute("inert");
    expect(background).not.toHaveAttribute("aria-hidden");
    background.remove();
  });

  it("leaves an already-inert sibling inert when it closes", () => {
    const background = withBackground();
    background.setAttribute("inert", "");
    background.setAttribute("aria-hidden", "true");
    const { rerender } = render(
      <SourceViewer open document={makeDocument()} lines={BODY} onClose={vi.fn()} />,
    );
    rerender(<SourceViewer open={false} document={null} lines={[]} onClose={vi.fn()} />);
    expect(background).toHaveAttribute("inert");
    expect(background).toHaveAttribute("aria-hidden", "true");
    background.remove();
  });

  it("closes on Escape and restores focus to whatever opened it", async () => {
    const onClose = vi.fn();
    const opener = document.createElement("button");
    document.body.appendChild(opener);
    opener.focus();

    const { rerender } = render(
      <SourceViewer open document={makeDocument()} lines={BODY} onClose={onClose} />,
    );
    expect(document.activeElement).not.toBe(opener);

    fireEvent.keyDown(window, { key: "Escape" });
    expect(onClose).toHaveBeenCalledOnce();

    rerender(<SourceViewer open={false} document={null} lines={[]} onClose={onClose} />);
    await waitFor(() => expect(document.activeElement).toBe(opener));
    opener.remove();
  });

  it("puts initial focus on the code region and keeps Tab inside", () => {
    render(<SourceViewer open document={makeDocument()} lines={BODY} onClose={vi.fn()} />);
    expect(document.activeElement).toBe(screen.getByTestId("source-viewer-code"));

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

  it("exposes the code region as a labelled, read-only group", () => {
    render(<SourceViewer open document={makeDocument()} lines={BODY} onClose={vi.fn()} />);
    expect(
      screen.getByRole("group", { name: /src\/main\.rs, 4 lines loaded, read only/ }),
    ).toBeInTheDocument();
  });

  // --- search ------------------------------------------------------------

  it("moves focus to search on the find shortcut and counts matches", () => {
    render(<SourceViewer open document={makeDocument()} lines={BODY} onClose={vi.fn()} />);
    fireEvent.keyDown(window, { key: "f", metaKey: true });
    const search = screen.getByTestId("source-viewer-search");
    expect(document.activeElement).toBe(search);

    fireEvent.change(search, { target: { value: "greeting" } });
    expect(screen.getByTestId("source-viewer-match-count")).toHaveTextContent("1/2");
    fireEvent.keyDown(search, { key: "Enter" });
    expect(screen.getByTestId("source-viewer-live")).toHaveTextContent("Match 1 of 2 for greeting");
    fireEvent.keyDown(search, { key: "Enter", shiftKey: true });
    expect(screen.getByTestId("source-viewer-live")).toHaveTextContent("Match 2 of 2 for greeting");
  });

  it("marks the active match differently from the rest", () => {
    render(<SourceViewer open document={makeDocument()} lines={BODY} onClose={vi.fn()} />);
    const search = screen.getByTestId("source-viewer-search");
    fireEvent.change(search, { target: { value: "greeting" } });
    fireEvent.keyDown(search, { key: "Enter" });

    const marks = screen.getAllByText("greeting");
    expect(marks.filter((mark) => mark.classList.contains("is-active"))).toHaveLength(1);
  });

  it("searches astral text without splitting a character", () => {
    const emoji = lines("a🎯b", "🎯🎯");
    render(
      <SourceViewer
        open
        document={makeDocument({ chunk: { ...makeDocument().chunk, lines: emoji } })}
        lines={emoji}
        onClose={vi.fn()}
      />,
    );
    fireEvent.change(screen.getByTestId("source-viewer-search"), { target: { value: "🎯" } });
    expect(screen.getByTestId("source-viewer-match-count")).toHaveTextContent("1/3");
    for (const mark of screen.getAllByText("🎯")) {
      expect(mark.textContent).toBe("🎯");
    }
  });

  it("says so when a search finds nothing", () => {
    render(<SourceViewer open document={makeDocument()} lines={BODY} onClose={vi.fn()} />);
    const search = screen.getByTestId("source-viewer-search");
    fireEvent.change(search, { target: { value: "absent" } });
    fireEvent.keyDown(search, { key: "Enter" });
    expect(screen.getByTestId("source-viewer-live")).toHaveTextContent("No matches for absent");
  });

  // --- ranges ------------------------------------------------------------

  it("marks every line of a multi-line range with its position", () => {
    render(
      <SourceViewer
        open
        document={makeDocument()}
        lines={BODY}
        highlightRange={{ firstLine: 2, lastLine: 4 }}
        onClose={vi.fn()}
      />,
    );
    expect(screen.getByTestId("source-line-1")).toHaveAttribute("data-range", "outside");
    expect(screen.getByTestId("source-line-2")).toHaveAttribute("data-range", "first");
    expect(screen.getByTestId("source-line-3")).toHaveAttribute("data-range", "middle");
    expect(screen.getByTestId("source-line-4")).toHaveAttribute("data-range", "last");
  });

  it("marks a single-line range as its own shape", () => {
    render(
      <SourceViewer
        open
        document={makeDocument()}
        lines={BODY}
        highlightRange={{ firstLine: 3, lastLine: 3 }}
        onClose={vi.fn()}
      />,
    );
    expect(screen.getByTestId("source-line-3")).toHaveAttribute("data-range", "only");
  });

  it("scrolls the opened line into view", () => {
    render(
      <SourceViewer open document={makeDocument()} lines={BODY} initialLine={3} onClose={vi.fn()} />,
    );
    expect(Element.prototype.scrollIntoView).toHaveBeenCalled();
    expect(screen.getByTestId("source-viewer-live")).toHaveTextContent("src/main.rs, line 3");
  });

  // --- copy scopes -------------------------------------------------------

  it("labels the copy control by what it will actually copy", async () => {
    const onCopy = vi.fn().mockResolvedValue(undefined);
    const paged = makeDocument({
      byteLen: 400,
      chunk: {
        ...makeDocument().chunk,
        eof: false,
        continuesNext: true,
        bytesConsumed: 64,
        nextCursor: {
          byteOffset: 64,
          nextLineNumber: 4,
          carryHex: "",
          continuesLine: true,
          documentDigest: DIGEST_A,
        },
      },
    });
    const { rerender } = render(
      <SourceViewer open document={paged} lines={BODY} onClose={vi.fn()} onCopy={onCopy} />,
    );
    const partial = screen.getByTestId("source-viewer-copy-loaded");
    expect(partial).toHaveTextContent("Copy 4 loaded lines");
    expect(onCopy).not.toHaveBeenCalled();
    expect(partial).toHaveAttribute(
      "title",
      "Copies only the lines loaded so far, not the whole file",
    );

    rerender(
      <SourceViewer open document={makeDocument()} lines={BODY} onClose={vi.fn()} onCopy={onCopy} />,
    );
    expect(screen.getByTestId("source-viewer-copy-loaded")).toHaveTextContent("Copy whole file");
  });

  it("offers a separate control for the highlighted range", async () => {
    const onCopy = vi.fn().mockResolvedValue(undefined);
    render(
      <SourceViewer
        open
        document={makeDocument()}
        lines={BODY}
        highlightRange={{ firstLine: 2, lastLine: 3 }}
        onClose={vi.fn()}
        onCopy={onCopy}
      />,
    );
    const rangeButton = screen.getByTestId("source-viewer-copy-range");
    expect(rangeButton).toHaveTextContent("Copy lines 2–3");
    fireEvent.click(rangeButton);
    await waitFor(() =>
      expect(onCopy).toHaveBeenCalledWith(
        '    let greeting = "hello";\n    println!(greeting);',
      ),
    );
    await waitFor(() =>
      expect(screen.getByTestId("source-viewer-live")).toHaveTextContent("Copied lines 2 to 3"),
    );
  });

  it("reports a failed copy instead of silently doing nothing", async () => {
    const onCopy = vi.fn().mockRejectedValue(new Error("denied"));
    render(
      <SourceViewer open document={makeDocument()} lines={BODY} onClose={vi.fn()} onCopy={onCopy} />,
    );
    fireEvent.click(screen.getByTestId("source-viewer-copy-loaded"));
    await waitFor(() =>
      expect(screen.getByTestId("source-viewer-copy-loaded")).toHaveTextContent("Copy failed"),
    );
    expect(screen.getByTestId("source-viewer-live")).toHaveTextContent(/copy it manually/i);
  });

  // --- projection honesty ------------------------------------------------

  it("says when a classification only saw a prefix", () => {
    render(
      <SourceViewer
        open
        lines={BODY}
        document={makeDocument({
          content: { verdict: "text", scannedBytes: 1_048_576, completeScan: false },
        })}
        onClose={vi.fn()}
      />,
    );
    expect(screen.getByTestId("source-viewer-notice")).toHaveTextContent(
      "classified from the first 1048576 bytes",
    );
  });

  it("says when identity is pinned rather than content-addressed", () => {
    render(
      <SourceViewer
        open
        lines={BODY}
        document={makeDocument({
          identity: { kind: "pinned", digest: DIGEST_A, stability: "heuristic" },
        })}
        onClose={vi.fn()}
      />,
    );
    expect(screen.getByTestId("source-viewer-notice")).toHaveTextContent(
      /a replaced file may not be detected/,
    );
  });

  it("refuses to render a binary file as text", () => {
    render(
      <SourceViewer
        open
        lines={[]}
        document={makeDocument({
          byteLen: 2048,
          content: { verdict: "binary", scannedBytes: 2048, completeScan: true },
          chunk: { ...makeDocument().chunk, lines: [], bytesConsumed: 0, eol: "none" },
        })}
        onClose={vi.fn()}
      />,
    );
    expect(screen.getByTestId("source-viewer-binary")).toHaveTextContent("2048 bytes of binary");
    expect(screen.queryByTestId("source-viewer-code")).not.toBeInTheDocument();
    expect(screen.getByTestId("source-viewer-copy-loaded")).toBeDisabled();
  });

  // --- paging and refusal ------------------------------------------------

  it("pages with an explicit control and reports progress", () => {
    const onLoadMore = vi.fn();
    const paged = makeDocument({
      byteLen: 400,
      chunk: {
        ...makeDocument().chunk,
        eof: false,
        continuesNext: true,
        bytesConsumed: 100,
        nextCursor: {
          byteOffset: 100,
          nextLineNumber: 4,
          carryHex: "",
          continuesLine: true,
          documentDigest: DIGEST_A,
        },
      },
    });
    render(
      <SourceViewer
        open
        document={paged}
        lines={BODY}
        hasMore
        onLoadMore={onLoadMore}
        onClose={vi.fn()}
      />,
    );
    expect(screen.getByTestId("source-viewer-progress")).toHaveTextContent("25% of 400 bytes");
    fireEvent.click(screen.getByTestId("source-viewer-load-more"));
    expect(onLoadMore).toHaveBeenCalledOnce();
  });

  it("explains a containment refusal in plain language and offers a retry", () => {
    const onRetry = vi.fn();
    render(
      <SourceViewer
        open
        document={null}
        lines={[]}
        error={new Error("symlink_rejected: `link` is a symbolic link")}
        onClose={vi.fn()}
        onRetry={onRetry}
      />,
    );
    const alert = screen.getByTestId("source-viewer-error");
    expect(alert).toHaveTextContent("crosses a link");
    fireEvent.click(within(alert).getByRole("button", { name: "Try again" }));
    expect(onRetry).toHaveBeenCalledOnce();
  });

  it("asks the reader to choose when more than one root matched", () => {
    const onChooseRoot = vi.fn();
    const second = { ...WORKSPACE_ROOT, token: `sv1.${SNAP}.1.${"1".repeat(32)}`, pathDigest: DIGEST_C, label: "repo/other" };
    render(
      <SourceViewer
        open
        document={null}
        lines={[]}
        rootChoice={{ candidates: [WORKSPACE_ROOT, second] }}
        onChooseRoot={onChooseRoot}
        onClose={vi.fn()}
      />,
    );
    expect(screen.getByRole("group", { name: "Choose a workspace" })).toBeInTheDocument();
    fireEvent.click(screen.getByTestId("source-viewer-choice-cccccccccccc"));
    expect(onChooseRoot).toHaveBeenCalledWith(second.token);
  });

  it("shows a loading state before any bytes arrive", () => {
    render(<SourceViewer open document={null} lines={[]} loading onClose={vi.fn()} />);
    expect(screen.getByText("Reading file…")).toBeInTheDocument();
  });

  it("marks a truncated line rather than hiding the cut", () => {
    const cut = [{ number: 1, text: "x".repeat(16), truncated: true }];
    render(
      <SourceViewer
        open
        document={makeDocument({ chunk: { ...makeDocument().chunk, lines: cut } })}
        lines={cut}
        onClose={vi.fn()}
      />,
    );
    expect(screen.getByTestId("source-line-1")).toHaveTextContent("⋯");
  });
});
