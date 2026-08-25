import { act, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { SOURCE_VIEW_CONTRACT, SOURCE_VIEW_REPLAY_POLICY } from "./sourceView";
import type { SourceViewTransport } from "./sourceViewTransport";
import { useSourceViewer } from "./useSourceViewer";

const SNAP = "0123456789abcdef0123456789abcdef";
const SNAP_2 = "fedcba9876543210fedcba9876543210";
const DIGEST_A = `${"0".repeat(63)}1`;
const DIGEST_B = `${"a".repeat(63)}b`;
const DIGEST_C = `${"c".repeat(63)}d`;
const WORKSPACE_TOKEN = `sv1.${SNAP}.0.00112233445566778899aabbccddeeff`;
const WORKTREE_TOKEN = `sv1.${SNAP}.1.ffeeddccbbaa99887766554433221100`;
const SECOND_WORKSPACE_TOKEN = `sv1.${SNAP}.2.0f1e2d3c4b5a69788796a5b4c3d2e1f0`;

const workspaceRoot = {
  token: WORKSPACE_TOKEN,
  kind: "workspace" as const,
  label: "repo/project",
  pathDigest: DIGEST_A,
  identityDigest: DIGEST_B,
  runId: null,
};
const worktreeRoot = {
  token: WORKTREE_TOKEN,
  kind: "isolated_worktree" as const,
  label: "runs/run-7",
  pathDigest: DIGEST_C,
  identityDigest: DIGEST_A,
  runId: "run-7",
};
const otherWorkspaceRoot = {
  ...workspaceRoot,
  token: SECOND_WORKSPACE_TOKEN,
  label: "repo/other",
  pathDigest: DIGEST_C,
};

function snapshot(roots = [workspaceRoot, worktreeRoot], snapshotId = SNAP) {
  return {
    snapshotId,
    revision: 1,
    issuedAtMs: 1_700_000_000_000,
    expiresAtMs: 1_700_000_900_000,
    principalFingerprint: DIGEST_A,
    policyFingerprint: DIGEST_B,
    replayPolicy: SOURCE_VIEW_REPLAY_POLICY,
    roots,
  };
}

function document(options: { token?: string; lines?: string[]; cursor?: boolean } = {}) {
  const lines = (options.lines ?? ["fn main() {}"]).map((text, index) => ({
    number: index + 1,
    text,
    truncated: false,
  }));
  const cursor = options.cursor
    ? {
        byteOffset: 13,
        nextLineNumber: lines.length + 1,
        carryHex: "",
        continuesLine: false,
        documentDigest: DIGEST_A,
      }
    : null;
  return {
    contract: SOURCE_VIEW_CONTRACT,
    root: options.token === WORKTREE_TOKEN ? worktreeRoot : workspaceRoot,
    snapshotId: SNAP,
    revision: 1,
    relativePath: "src/main.rs",
    language: "rust",
    byteLen: 26,
    content: { verdict: "text" as const, scannedBytes: 26, completeScan: true },
    identity: { kind: "content" as const, digest: DIGEST_A },
    limits: { maxBytes: 524_288, maxLines: 1_200, maxLineChars: 2_000 },
    chunk: {
      lines,
      startByte: 0,
      bytesConsumed: 13,
      lossyReplacements: 0,
      eol: "lf" as const,
      continuesPrevious: false,
      continuesNext: false,
      nextCursor: cursor,
      eof: !options.cursor,
    },
  };
}

const snapshotFn = vi.fn();
const readFn = vi.fn();
const revokeFn = vi.fn();

const transport: SourceViewTransport = {
  channel: "tauri",
  snapshot: (request) => snapshotFn(request),
  read: (request) => readFn(request),
  revoke: (snapshotId) => revokeFn(snapshotId),
};

const NOW = 1_700_000_100_000;

function useViewer(sessionId: string | null = "session-1") {
  return useSourceViewer(sessionId, { transport, now: () => NOW });
}

beforeEach(() => {
  snapshotFn.mockReset().mockResolvedValue(snapshot());
  readFn.mockReset().mockResolvedValue(document());
  revokeFn.mockReset().mockResolvedValue(true);
});

afterEach(() => vi.clearAllMocks());

describe("useSourceViewer", () => {
  it("starts closed and touches no boundary", () => {
    const { result } = renderHook(() => useViewer());
    expect(result.current.open).toBe(false);
    expect(snapshotFn).not.toHaveBeenCalled();
    expect(readFn).not.toHaveBeenCalled();
  });

  it("snapshots, selects one root, and reads through its token", async () => {
    const { result } = renderHook(() => useViewer());
    act(() => result.current.openSource("src/main.rs", 3));

    await waitFor(() => expect(result.current.document).not.toBeNull());
    expect(snapshotFn).toHaveBeenCalledWith({ sessionId: "session-1" });
    expect(readFn).toHaveBeenCalledWith({
      token: WORKSPACE_TOKEN,
      path: "src/main.rs",
      sessionId: "session-1",
    });
    expect(result.current.lines.map((line) => line.text)).toEqual(["fn main() {}"]);
    expect(result.current.request?.line).toBe(3);
  });

  it("reads a run's file from that run's own worktree token", async () => {
    readFn.mockResolvedValue(document({ token: WORKTREE_TOKEN }));
    const { result } = renderHook(() => useViewer());
    act(() => result.current.openSource("src/main.rs", 1, { by: "run", runId: "run-7" }));

    await waitFor(() => expect(readFn).toHaveBeenCalled());
    expect(readFn.mock.calls[0][0].token).toBe(WORKTREE_TOKEN);
  });

  it("refuses rather than falling back when a run has no worktree", async () => {
    const { result } = renderHook(() => useViewer());
    act(() => result.current.openSource("src/main.rs", 1, { by: "run", runId: "run-absent" }));

    await waitFor(() => expect(result.current.error).toBeTruthy());
    expect(String(result.current.error)).toContain("unknown_root");
    expect(readFn).not.toHaveBeenCalled();
  });

  it("never picks the first workspace when several match", async () => {
    snapshotFn.mockResolvedValue(snapshot([workspaceRoot, otherWorkspaceRoot]));
    const { result } = renderHook(() => useViewer());
    act(() => result.current.openSource("src/main.rs"));

    await waitFor(() => expect(result.current.choice).not.toBeNull());
    expect(result.current.choice?.candidates.map((root) => root.token)).toEqual([
      WORKSPACE_TOKEN,
      SECOND_WORKSPACE_TOKEN,
    ]);
    expect(readFn).not.toHaveBeenCalled();
  });

  it("reads only after the reader chooses one of the ambiguous roots", async () => {
    snapshotFn.mockResolvedValue(snapshot([workspaceRoot, otherWorkspaceRoot]));
    const { result } = renderHook(() => useViewer());
    act(() => result.current.openSource("src/main.rs"));
    await waitFor(() => expect(result.current.choice).not.toBeNull());

    act(() => result.current.chooseRoot(SECOND_WORKSPACE_TOKEN));
    await waitFor(() => expect(readFn).toHaveBeenCalled());
    expect(readFn.mock.calls[0][0].token).toBe(SECOND_WORKSPACE_TOKEN);
    expect(result.current.choice).toBeNull();
  });

  it("reports having no approved workspace at all", async () => {
    snapshotFn.mockResolvedValue(snapshot([]));
    const { result } = renderHook(() => useViewer(null));
    act(() => result.current.openSource("src/main.rs"));

    await waitFor(() => expect(String(result.current.error)).toContain("no_approved_root"));
    expect(readFn).not.toHaveBeenCalled();
  });

  it("reuses a live snapshot across reads rather than re-issuing one", async () => {
    const { result } = renderHook(() => useViewer());
    act(() => result.current.openSource("src/main.rs"));
    await waitFor(() => expect(result.current.document).not.toBeNull());
    act(() => result.current.openSource("src/other.rs"));
    await waitFor(() => expect(readFn).toHaveBeenCalledTimes(2));
    expect(snapshotFn).toHaveBeenCalledTimes(1);
  });

  it("re-issues the snapshot once on an authorization refusal and retries", async () => {
    readFn
      .mockRejectedValueOnce(new Error("token_expired: the source token has expired"))
      .mockResolvedValueOnce(document());
    snapshotFn
      .mockResolvedValueOnce(snapshot())
      .mockResolvedValueOnce(snapshot(undefined, SNAP_2));

    const { result } = renderHook(() => useViewer());
    act(() => result.current.openSource("src/main.rs"));

    await waitFor(() => expect(result.current.document).not.toBeNull());
    expect(snapshotFn).toHaveBeenCalledTimes(2);
    expect(readFn).toHaveBeenCalledTimes(2);
    expect(result.current.error).toBeNull();
  });

  it("surfaces a second authorization refusal instead of looping", async () => {
    readFn.mockRejectedValue(new Error("policy_drift: authorization changed"));
    const { result } = renderHook(() => useViewer());
    act(() => result.current.openSource("src/main.rs"));

    await waitFor(() => expect(result.current.error).toBeTruthy());
    expect(readFn).toHaveBeenCalledTimes(2);
    expect(snapshotFn).toHaveBeenCalledTimes(2);
    expect(String(result.current.error)).toContain("policy_drift");
  });

  it("does not re-snapshot for a containment refusal", async () => {
    readFn.mockRejectedValue(new Error("parent_escape: walks above the root"));
    const { result } = renderHook(() => useViewer());
    act(() => result.current.openSource("../../etc/passwd"));

    await waitFor(() => expect(result.current.error).toBeTruthy());
    expect(readFn).toHaveBeenCalledTimes(1);
    expect(snapshotFn).toHaveBeenCalledTimes(1);
  });

  it("pages with the cursor and rejoins the lines", async () => {
    readFn
      .mockResolvedValueOnce(document({ lines: ["first"], cursor: true }))
      .mockResolvedValueOnce({
        ...document({ lines: ["second"] }),
        chunk: {
          ...document({ lines: ["second"] }).chunk,
          lines: [{ number: 2, text: "second", truncated: false }],
          startByte: 13,
          continuesPrevious: false,
        },
      });

    const { result } = renderHook(() => useViewer());
    act(() => result.current.openSource("src/main.rs"));
    await waitFor(() => expect(result.current.hasMore).toBe(true));

    act(() => result.current.loadMore());
    await waitFor(() => expect(result.current.lines).toHaveLength(2));
    expect(result.current.lines.map((line) => line.text)).toEqual(["first", "second"]);
    expect(readFn.mock.calls[1][0].cursor?.byteOffset).toBe(13);
    expect(result.current.hasMore).toBe(false);
  });

  it("keeps already-loaded pages when a later page fails", async () => {
    readFn
      .mockResolvedValueOnce(document({ lines: ["first"], cursor: true }))
      .mockRejectedValueOnce(new Error("io: unavailable"));

    const { result } = renderHook(() => useViewer());
    act(() => result.current.openSource("src/main.rs"));
    await waitFor(() => expect(result.current.hasMore).toBe(true));

    act(() => result.current.loadMore());
    await waitFor(() => expect(result.current.error).toBeTruthy());
    expect(result.current.lines.map((line) => line.text)).toEqual(["first"]);
  });

  it("ignores a slow read that lands after a newer one", async () => {
    let release: (value: unknown) => void = () => {};
    readFn
      .mockImplementationOnce(() => new Promise((resolve) => (release = resolve)))
      .mockResolvedValueOnce(document({ lines: ["second file"] }));

    const { result } = renderHook(() => useViewer());
    act(() => result.current.openSource("src/first.rs"));
    act(() => result.current.openSource("src/second.rs"));

    await waitFor(() => expect(result.current.lines).toHaveLength(1));
    expect(result.current.lines[0].text).toBe("second file");
    await act(async () => {
      release(document({ lines: ["first file"] }));
    });
    expect(result.current.lines[0].text).toBe("second file");
  });

  it("closing forgets the document, the lines, the refusal, and the request", async () => {
    const { result } = renderHook(() => useViewer());
    act(() => result.current.openSource("src/main.rs"));
    await waitFor(() => expect(result.current.document).not.toBeNull());

    act(() => result.current.close());
    expect(result.current.open).toBe(false);
    expect(result.current.document).toBeNull();
    expect(result.current.lines).toEqual([]);
    expect(result.current.error).toBeNull();
    expect(result.current.request).toBeNull();
  });

  it("retries the same request without a fresh open", async () => {
    readFn
      .mockRejectedValueOnce(new Error("io: unavailable"))
      .mockResolvedValueOnce(document());
    const { result } = renderHook(() => useViewer());
    act(() => result.current.openSource("src/main.rs"));
    await waitFor(() => expect(result.current.error).toBeTruthy());

    act(() => result.current.retry());
    await waitFor(() => expect(result.current.document).not.toBeNull());
    expect(result.current.error).toBeNull();
  });

  it("never revokes or mutates anything as a side effect of reading", async () => {
    const { result } = renderHook(() => useViewer());
    act(() => result.current.openSource("src/main.rs"));
    await waitFor(() => expect(result.current.document).not.toBeNull());
    act(() => result.current.close());
    expect(revokeFn).not.toHaveBeenCalled();
  });
});
