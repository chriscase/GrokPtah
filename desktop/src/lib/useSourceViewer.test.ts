import { act, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const sourceViewRoots = vi.fn();
const sourceViewOpen = vi.fn();

vi.mock("./api", () => ({
  api: {
    sourceViewRoots: (...args: unknown[]) => sourceViewRoots(...args),
    sourceViewOpen: (...args: unknown[]) => sourceViewOpen(...args),
  },
}));

const { useSourceViewer } = await import("./useSourceViewer");

const WORKSPACE = {
  id: "ws-1111111111111111",
  kind: "workspace" as const,
  label: "repo/project",
  path: "/approved/repo/project",
  runId: null,
};

const WORKTREE = {
  id: "wt-2222222222222222",
  kind: "isolated_worktree" as const,
  label: "run run-7 worktree",
  path: "/approved/repo/project/.grokptah/worktrees/runs/run-7",
  runId: "run-7",
};

const DOCUMENT = {
  rootId: WORKSPACE.id,
  rootKind: "workspace" as const,
  rootPath: WORKSPACE.path,
  rootLabel: WORKSPACE.label,
  runId: null,
  relativePath: "src/main.rs",
  absolutePath: "/approved/repo/project/src/main.rs",
  language: "rust",
  encoding: "utf8" as const,
  byteLen: 12,
  bytesRead: 12,
  lines: [{ number: 1, text: "fn main() {}", truncated: false }],
  lineCount: 1,
  truncatedBytes: false,
  truncatedLines: false,
  lossyReplacements: 0,
  eol: "lf" as const,
  contentFingerprint: "fnv1a64:3333333333333333",
};

beforeEach(() => {
  sourceViewRoots.mockReset().mockResolvedValue([WORKSPACE, WORKTREE]);
  sourceViewOpen.mockReset().mockResolvedValue(DOCUMENT);
});

afterEach(() => vi.clearAllMocks());

describe("useSourceViewer", () => {
  it("starts closed and reads nothing", () => {
    const { result } = renderHook(() => useSourceViewer("session-1"));
    expect(result.current.open).toBe(false);
    expect(sourceViewRoots).not.toHaveBeenCalled();
  });

  it("opens a workspace file through the approved boundary", async () => {
    const { result } = renderHook(() => useSourceViewer("session-1"));

    act(() => result.current.openSource("src/main.rs", 3));

    expect(result.current.open).toBe(true);
    await waitFor(() => expect(result.current.document).toEqual(DOCUMENT));
    expect(sourceViewRoots).toHaveBeenCalledWith("session-1");
    expect(sourceViewOpen).toHaveBeenCalledWith(WORKSPACE.id, "src/main.rs", {
      sessionId: "session-1",
    });
    expect(result.current.request).toEqual({
      path: "src/main.rs",
      line: 3,
      preference: {},
    });
  });

  it("reads a run's file from that run's own worktree", async () => {
    const { result } = renderHook(() => useSourceViewer("session-1"));

    act(() => result.current.openSource("src/main.rs", 1, { runId: "run-7" }));

    await waitFor(() => expect(sourceViewOpen).toHaveBeenCalled());
    expect(sourceViewOpen).toHaveBeenCalledWith(WORKTREE.id, "src/main.rs", {
      sessionId: "session-1",
    });
  });

  it("refuses rather than reading the shared workspace for an unknown run", async () => {
    const { result } = renderHook(() => useSourceViewer("session-1"));

    act(() => result.current.openSource("src/main.rs", 1, { runId: "run-absent" }));

    await waitFor(() => expect(result.current.error).toBeInstanceOf(Error));
    expect(String(result.current.error)).toContain("unknown_root");
    expect(sourceViewOpen).not.toHaveBeenCalled();
  });

  it("reports having no approved workspace at all", async () => {
    sourceViewRoots.mockResolvedValue([]);
    const { result } = renderHook(() => useSourceViewer(null));

    act(() => result.current.openSource("src/main.rs"));

    await waitFor(() => expect(String(result.current.error)).toContain("no_approved_root"));
  });

  it("surfaces a boundary refusal and clears any stale document", async () => {
    const { result } = renderHook(() => useSourceViewer("session-1"));
    act(() => result.current.openSource("src/main.rs"));
    await waitFor(() => expect(result.current.document).toEqual(DOCUMENT));

    sourceViewOpen.mockRejectedValue(new Error("parent_escape: walks above the root"));
    act(() => result.current.openSource("../../etc/passwd"));

    await waitFor(() => expect(String(result.current.error)).toContain("parent_escape"));
    expect(result.current.document).toBeNull();
  });

  it("retries the same request without a fresh open", async () => {
    sourceViewOpen.mockRejectedValueOnce(new Error("io: transient"));
    const { result } = renderHook(() => useSourceViewer("session-1"));

    act(() => result.current.openSource("src/main.rs"));
    await waitFor(() => expect(result.current.error).toBeTruthy());

    act(() => result.current.retry());
    await waitFor(() => expect(result.current.document).toEqual(DOCUMENT));
    expect(result.current.error).toBeNull();
  });

  it("ignores a slow first read that lands after a second one", async () => {
    const second = { ...DOCUMENT, relativePath: "src/second.rs" };
    let releaseFirst: (value: unknown) => void = () => {};
    sourceViewOpen
      .mockImplementationOnce(
        () =>
          new Promise((resolve) => {
            releaseFirst = resolve;
          }),
      )
      .mockResolvedValueOnce(second);

    const { result } = renderHook(() => useSourceViewer("session-1"));
    act(() => result.current.openSource("src/first.rs"));
    act(() => result.current.openSource("src/second.rs"));

    await waitFor(() => expect(result.current.document).toEqual(second));
    await act(async () => {
      releaseFirst(DOCUMENT);
    });
    expect(result.current.document).toEqual(second);
  });

  it("closing forgets the document, the refusal, and the request", async () => {
    const { result } = renderHook(() => useSourceViewer("session-1"));
    act(() => result.current.openSource("src/main.rs"));
    await waitFor(() => expect(result.current.document).toEqual(DOCUMENT));

    act(() => result.current.close());

    expect(result.current.open).toBe(false);
    expect(result.current.document).toBeNull();
    expect(result.current.error).toBeNull();
    expect(result.current.request).toBeNull();
  });
});
