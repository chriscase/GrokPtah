import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { RunDiffNavigator } from "./RunDiffNavigator";

afterEach(cleanup);

const TWO_FILES = [
  "diff --git a/src/lib/api.ts b/src/lib/api.ts",
  "--- a/src/lib/api.ts",
  "+++ b/src/lib/api.ts",
  "@@ -10,3 +10,4 @@ export const api = {",
  "   fileTree: () => invoke('file_tree'),",
  "-  legacy: () => invoke('legacy'),",
  "+  sourceViewOpen: () => invoke('source_view_open'),",
  "+  sourceViewRoots: () => invoke('source_view_roots'),",
  "diff --git a/src/new.ts b/src/new.ts",
  "--- /dev/null",
  "+++ b/src/new.ts",
  "@@ -0,0 +1,1 @@",
  "+export const created = true;",
].join("\n");

describe("RunDiffNavigator", () => {
  it("summarises the review across files", () => {
    render(<RunDiffNavigator runId="run-1" diff={TWO_FILES} />);
    expect(screen.getByTestId("run-diff-summary")).toHaveTextContent("2 files · +3 −1");
  });

  it("says when the diff was capped rather than implying it is whole", () => {
    render(<RunDiffNavigator runId="run-1" diff={TWO_FILES} truncated />);
    expect(screen.getByTestId("run-diff-summary")).toHaveTextContent("diff truncated");
  });

  it("shows one file at a time with its position in the review", () => {
    render(<RunDiffNavigator runId="run-1" diff={TWO_FILES} />);
    expect(screen.getByTestId("run-diff-position")).toHaveTextContent(
      "File 1 of 2 · src/lib/api.ts · modified · +2 −1",
    );
  });

  it("steps between files with the next and previous controls", () => {
    render(<RunDiffNavigator runId="run-1" diff={TWO_FILES} />);
    expect(screen.getByTestId("run-diff-prev")).toBeDisabled();

    fireEvent.click(screen.getByTestId("run-diff-next"));
    expect(screen.getByTestId("run-diff-position")).toHaveTextContent(
      "File 2 of 2 · src/new.ts · added",
    );
    expect(screen.getByTestId("run-diff-next")).toBeDisabled();

    fireEvent.click(screen.getByTestId("run-diff-prev"));
    expect(screen.getByTestId("run-diff-position")).toHaveTextContent("File 1 of 2");
  });

  it("jumps to a file from the labelled picker", () => {
    render(<RunDiffNavigator runId="run-1" diff={TWO_FILES} />);
    fireEvent.change(screen.getByRole("combobox", { name: "Changed file" }), {
      target: { value: "1" },
    });
    expect(screen.getByTestId("run-diff-position")).toHaveTextContent("src/new.ts");
  });

  it("renders hunk lines with their new-side numbers", () => {
    render(<RunDiffNavigator runId="run-1" diff={TWO_FILES} />);
    const hunks = screen.getByTestId("run-diff-hunks");
    expect(within(hunks).getByText(/Lines 10–13/)).toBeInTheDocument();
    expect(within(hunks).getAllByTestId("run-diff-line-add")).toHaveLength(2);
    expect(within(hunks).getAllByTestId("run-diff-line-remove")).toHaveLength(1);
  });

  it("names each change kind for a screen reader, not only by colour", () => {
    render(<RunDiffNavigator runId="run-1" diff={TWO_FILES} />);
    const added = screen.getAllByTestId("run-diff-line-add")[0];
    expect(added).toHaveTextContent(/Added line 11:/);
    const removed = screen.getAllByTestId("run-diff-line-remove")[0];
    expect(removed).toHaveTextContent(/Removed line 11:/);
  });

  it("opens the selected file in the run's own isolated worktree at its first change", () => {
    const onOpenFile = vi.fn();
    render(<RunDiffNavigator runId="run-7" diff={TWO_FILES} onOpenFile={onOpenFile} />);

    fireEvent.click(screen.getByTestId("run-diff-open"));
    expect(onOpenFile).toHaveBeenCalledWith("src/lib/api.ts", 11);

    fireEvent.click(screen.getByTestId("run-diff-next"));
    fireEvent.click(screen.getByTestId("run-diff-open"));
    expect(onOpenFile).toHaveBeenLastCalledWith("src/new.ts", 1);
  });

  it("names the worktree it would open in", () => {
    render(<RunDiffNavigator runId="run-7" diff={TWO_FILES} onOpenFile={vi.fn()} />);
    expect(screen.getByTestId("run-diff-open")).toHaveAttribute(
      "title",
      "Open src/lib/api.ts in the isolated worktree for run run-7",
    );
  });

  it("omits the open control when no worktree is inspectable", () => {
    render(<RunDiffNavigator runId="run-7" diff={TWO_FILES} />);
    expect(screen.queryByTestId("run-diff-open")).not.toBeInTheDocument();
  });

  it("refuses to offer a removed file for opening", () => {
    const diff = [
      "diff --git a/src/gone.ts b/src/gone.ts",
      "--- a/src/gone.ts",
      "+++ /dev/null",
      "@@ -1,1 +0,0 @@",
      "-const a = 1;",
    ].join("\n");
    render(<RunDiffNavigator runId="run-1" diff={diff} onOpenFile={vi.fn()} />);
    expect(screen.getByTestId("run-diff-open")).toBeDisabled();
  });

  it("refuses to render or open a binary file", () => {
    const diff = [
      "diff --git a/assets/icon.png b/assets/icon.png",
      "Binary files a/assets/icon.png and b/assets/icon.png differ",
    ].join("\n");
    render(<RunDiffNavigator runId="run-1" diff={diff} onOpenFile={vi.fn()} />);
    expect(screen.getByTestId("run-diff-binary")).toBeInTheDocument();
    expect(screen.getByTestId("run-diff-open")).toBeDisabled();
    expect(screen.queryByTestId("run-diff-hunks")).not.toBeInTheDocument();
  });

  it("says so for an empty review instead of showing an empty shell", () => {
    render(<RunDiffNavigator runId="run-1" diff="" />);
    expect(screen.getByTestId("run-diff-empty")).toHaveTextContent("No changes");
  });

  it("says so when a non-empty diff yields no files", () => {
    render(<RunDiffNavigator runId="run-1" diff="something that is not a diff" />);
    expect(screen.getByTestId("run-diff-empty")).toHaveTextContent(
      "No per-file changes could be read",
    );
  });

  it("resets to the first file when a fresh review arrives", () => {
    const { rerender } = render(<RunDiffNavigator runId="run-1" diff={TWO_FILES} />);
    fireEvent.click(screen.getByTestId("run-diff-next"));
    expect(screen.getByTestId("run-diff-position")).toHaveTextContent("File 2 of 2");

    rerender(
      <RunDiffNavigator
        runId="run-1"
        diff={["--- a/only.ts", "+++ b/only.ts", "@@ -1 +1 @@", "-a", "+b"].join("\n")}
      />,
    );
    expect(screen.getByTestId("run-diff-position")).toHaveTextContent("File 1 of 1 · only.ts");
  });

  it("labels the region by the run it belongs to", () => {
    render(<RunDiffNavigator runId="run-9" diff={TWO_FILES} />);
    expect(screen.getByRole("region", { name: "Changed files in run run-9" })).toBeInTheDocument();
  });
});
