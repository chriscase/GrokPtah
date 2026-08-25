import { describe, expect, it } from "vitest";
import {
  SOURCE_VIEW_CONTRACT,
  SOURCE_VIEW_ERROR_CODES,
  SOURCE_VIEW_REPLAY_POLICY,
  appendSourceChunk,
  digestLabel,
  isAuthorizationRefusal,
  isSnapshotLive,
  parseSourceChunk,
  parseSourceDocument,
  parseSourceReadCursor,
  parseSourceRootDescriptor,
  parseSourceRootSnapshot,
  parseSourceViewErrorCode,
  projectionNotice,
  readProgress,
  rootIdentityLabel,
  selectSourceRoot,
  shouldRefreshSnapshot,
  sourceViewErrorSummary,
  type SourceChunk,
  type SourceDocument,
  type SourceLine,
  type SourceRootDescriptor,
  type SourceRootSnapshot,
} from "./sourceView";

const SNAP = "0123456789abcdef0123456789abcdef";
const DIGEST_A = `${"0".repeat(63)}1`;
const DIGEST_B = `${"a".repeat(63)}b`;
const DIGEST_C = `${"c".repeat(63)}d`;
const TOKEN_0 = `sv1.${SNAP}.0.00112233445566778899aabbccddeeff`;
const TOKEN_1 = `sv1.${SNAP}.1.ffeeddccbbaa99887766554433221100`;

function workspaceRoot(overrides: Partial<SourceRootDescriptor> = {}): SourceRootDescriptor {
  return {
    token: TOKEN_0,
    kind: "workspace",
    label: "repo/project",
    pathDigest: DIGEST_A,
    identityDigest: DIGEST_B,
    runId: null,
    ...overrides,
  };
}

function worktreeRoot(overrides: Partial<SourceRootDescriptor> = {}): SourceRootDescriptor {
  return {
    token: TOKEN_1,
    kind: "isolated_worktree",
    label: "runs/run-7",
    pathDigest: DIGEST_C,
    identityDigest: DIGEST_A,
    runId: "run-7",
    ...overrides,
  };
}

function snapshot(overrides: Partial<SourceRootSnapshot> = {}): SourceRootSnapshot {
  return {
    snapshotId: SNAP,
    revision: 1,
    issuedAtMs: 1_700_000_000_000,
    expiresAtMs: 1_700_000_900_000,
    principalFingerprint: DIGEST_A,
    policyFingerprint: DIGEST_B,
    replayPolicy: SOURCE_VIEW_REPLAY_POLICY,
    roots: [workspaceRoot(), worktreeRoot()],
    ...overrides,
  };
}

function chunk(overrides: Partial<SourceChunk> = {}): SourceChunk {
  return {
    lines: [{ number: 1, text: "fn main() {}", truncated: false }],
    startByte: 0,
    bytesConsumed: 13,
    lossyReplacements: 0,
    eol: "lf",
    continuesPrevious: false,
    continuesNext: false,
    nextCursor: null,
    eof: true,
    ...overrides,
  };
}

function document(overrides: Partial<SourceDocument> = {}): SourceDocument {
  return {
    contract: SOURCE_VIEW_CONTRACT,
    root: workspaceRoot(),
    snapshotId: SNAP,
    revision: 1,
    relativePath: "src/main.rs",
    language: "rust",
    byteLen: 13,
    content: { verdict: "text", scannedBytes: 13, completeScan: true },
    identity: { kind: "content", digest: DIGEST_A },
    limits: { maxBytes: 524_288, maxLines: 1_200, maxLineChars: 2_000 },
    chunk: chunk(),
    ...overrides,
  };
}

describe("parseSourceRootDescriptor", () => {
  it("accepts a well-formed root", () => {
    expect(parseSourceRootDescriptor(workspaceRoot())).toEqual(workspaceRoot());
    expect(parseSourceRootDescriptor(worktreeRoot()).runId).toBe("run-7");
  });

  it("refuses a token that is not a source-view token", () => {
    for (const token of [
      "nope",
      `sv0.${SNAP}.0.00112233445566778899aabbccddeeff`,
      `sv1.${SNAP}.01.00112233445566778899aabbccddeeff`,
      `sv1.${SNAP}.0.tooshort`,
      `sv1.SHORT.0.00112233445566778899aabbccddeeff`,
    ]) {
      expect(() => parseSourceRootDescriptor(workspaceRoot({ token }))).toThrow(/token is malformed/);
    }
  });

  it("refuses digests that are not 32 bytes of hex", () => {
    expect(() => parseSourceRootDescriptor(workspaceRoot({ pathDigest: "abc" }))).toThrow(
      /pathDigest is malformed/,
    );
    expect(() =>
      parseSourceRootDescriptor(workspaceRoot({ identityDigest: DIGEST_B.toUpperCase() })),
    ).toThrow(/identityDigest is malformed/);
  });

  it("refuses an unknown kind", () => {
    expect(() => parseSourceRootDescriptor({ ...workspaceRoot(), kind: "anywhere" })).toThrow(
      /kind must be one of/,
    );
  });

  it("requires runId to be present, even as null", () => {
    const { runId: _runId, ...withoutRun } = workspaceRoot();
    expect(() => parseSourceRootDescriptor(withoutRun)).toThrow(/runId is required/);
  });

  it("refuses a payload that carries a location", () => {
    for (const key of ["path", "absolutePath", "rootPath", "workspacePath", "cwd"]) {
      expect(() =>
        parseSourceRootDescriptor({ ...workspaceRoot(), [key]: "/approved/repo" }),
      ).toThrow(new RegExp(`must not carry \`${key}\``));
    }
  });

  it("refuses any unexpected field", () => {
    expect(() => parseSourceRootDescriptor({ ...workspaceRoot(), extra: 1 })).toThrow(
      /unexpected field `extra`/,
    );
  });
});

describe("parseSourceRootSnapshot", () => {
  it("accepts a well-formed snapshot, including an empty one", () => {
    expect(parseSourceRootSnapshot(snapshot()).roots).toHaveLength(2);
    expect(parseSourceRootSnapshot(snapshot({ roots: [] })).roots).toEqual([]);
  });

  it("pins the replay policy", () => {
    expect(() =>
      parseSourceRootSnapshot({ ...snapshot(), replayPolicy: "anything-goes" }),
    ).toThrow(/unexpected replay policy/);
  });

  it("refuses a repeated root token", () => {
    expect(() =>
      parseSourceRootSnapshot(snapshot({ roots: [workspaceRoot(), workspaceRoot()] })),
    ).toThrow(/repeats a root token/);
  });

  it("refuses a revision below one and a non-integer timestamp", () => {
    expect(() => parseSourceRootSnapshot(snapshot({ revision: 0 }))).toThrow(/revision/);
    expect(() =>
      parseSourceRootSnapshot({ ...snapshot(), issuedAtMs: 1.5 }),
    ).toThrow(/issuedAtMs must be a safe integer/);
  });
});

describe("parseSourceReadCursor", () => {
  const cursor = {
    byteOffset: 512,
    nextLineNumber: 21,
    carryHex: "f09f8e",
    continuesLine: true,
    documentDigest: DIGEST_A,
  };

  it("accepts a well-formed cursor and an empty carry", () => {
    expect(parseSourceReadCursor(cursor)).toEqual(cursor);
    expect(parseSourceReadCursor({ ...cursor, carryHex: "" }).carryHex).toBe("");
  });

  it("refuses a carry longer than three bytes or not lowercase hex", () => {
    expect(() => parseSourceReadCursor({ ...cursor, carryHex: "f09f8eaf" })).toThrow(/carryHex/);
    expect(() => parseSourceReadCursor({ ...cursor, carryHex: "F0" })).toThrow(/carryHex/);
    expect(() => parseSourceReadCursor({ ...cursor, carryHex: "f" })).toThrow(/carryHex/);
  });

  it("refuses a zero line number and a negative offset", () => {
    expect(() => parseSourceReadCursor({ ...cursor, nextLineNumber: 0 })).toThrow(/nextLineNumber/);
    expect(() => parseSourceReadCursor({ ...cursor, byteOffset: -1 })).toThrow(/byteOffset/);
  });

  it("requires the continuation flag to be stated", () => {
    const { continuesLine: _flag, ...without } = cursor;
    expect(() => parseSourceReadCursor(without)).toThrow(/continuesLine must be a boolean/);
  });
});

describe("parseSourceChunk", () => {
  it("refuses non-consecutive line numbers", () => {
    expect(() =>
      parseSourceChunk(
        chunk({
          lines: [
            { number: 1, text: "a", truncated: false },
            { number: 3, text: "c", truncated: false },
          ],
        }),
      ),
    ).toThrow(/consecutive/);
  });

  it("refuses a finished chunk that still carries a cursor", () => {
    expect(() =>
      parseSourceChunk(
        chunk({
          eof: true,
          nextCursor: {
            byteOffset: 1,
            nextLineNumber: 1,
            carryHex: "",
            continuesLine: false,
            documentDigest: DIGEST_A,
          },
        }),
      ),
    ).toThrow(/must not carry a continuation cursor/);
  });

  it("refuses a continued chunk with no cursor", () => {
    expect(() => parseSourceChunk(chunk({ continuesNext: true, eof: false }))).toThrow(
      /must carry a continuation cursor/,
    );
  });

  it("refuses a chunk whose cursor disagrees about continuation", () => {
    expect(() =>
      parseSourceChunk(
        chunk({
          eof: false,
          continuesNext: true,
          nextCursor: {
            byteOffset: 4,
            nextLineNumber: 1,
            carryHex: "",
            continuesLine: false,
            documentDigest: DIGEST_A,
          },
        }),
      ),
    ).toThrow(/disagree about line continuation/);
  });
});

describe("parseSourceDocument", () => {
  it("accepts a well-formed document", () => {
    expect(parseSourceDocument(document()).relativePath).toBe("src/main.rs");
  });

  it("pins the contract id", () => {
    expect(() => parseSourceDocument({ ...document(), contract: "grokptah.source-view.v2" })).toThrow(
      /unexpected contract/,
    );
  });

  it("refuses a binary document that still carries text", () => {
    expect(() =>
      parseSourceDocument(
        document({ content: { verdict: "binary", scannedBytes: 5, completeScan: true } }),
      ),
    ).toThrow(/must not carry rendered lines/);
  });

  it("accepts a binary document with no lines", () => {
    const binary = parseSourceDocument(
      document({
        content: { verdict: "binary", scannedBytes: 5, completeScan: true },
        chunk: chunk({ lines: [], bytesConsumed: 0, eol: "none" }),
      }),
    );
    expect(binary.chunk.lines).toEqual([]);
  });

  it("refuses a relativePath that is not root-relative", () => {
    for (const relativePath of ["/etc/passwd", "C:\\repo\\x.rs"]) {
      expect(() => parseSourceDocument(document({ relativePath }))).toThrow(/root-relative/);
    }
  });

  it("refuses limits above the published ceilings", () => {
    expect(() =>
      parseSourceDocument(
        document({ limits: { maxBytes: 999_999_999, maxLines: 1_200, maxLineChars: 2_000 } }),
      ),
    ).toThrow(/maxBytes/);
  });

  it("refuses an identity that does not name its kind", () => {
    expect(() =>
      parseSourceDocument({ ...document(), identity: { digest: DIGEST_A } }),
    ).toThrow(/kind must be one of/);
  });

  it("accepts a pinned identity with its stability", () => {
    const pinned = parseSourceDocument(
      document({ identity: { kind: "pinned", digest: DIGEST_A, stability: "heuristic" } }),
    );
    expect(pinned.identity).toEqual({ kind: "pinned", digest: DIGEST_A, stability: "heuristic" });
  });
});

describe("selectSourceRoot", () => {
  it("resolves a token to exactly that root", () => {
    expect(selectSourceRoot(snapshot(), { by: "token", token: TOKEN_1 })).toEqual({
      kind: "resolved",
      root: worktreeRoot(),
    });
  });

  it("resolves a run to its own worktree", () => {
    expect(selectSourceRoot(snapshot(), { by: "run", runId: "run-7" })).toEqual({
      kind: "resolved",
      root: worktreeRoot(),
    });
  });

  it("refuses rather than falling back for an unknown run", () => {
    expect(selectSourceRoot(snapshot(), { by: "run", runId: "run-absent" })).toEqual({
      kind: "absent",
    });
  });

  it("never picks the first workspace when several match", () => {
    const second = workspaceRoot({ token: TOKEN_1, pathDigest: DIGEST_C, label: "other/tree" });
    const selection = selectSourceRoot(snapshot({ roots: [workspaceRoot(), second] }), {
      by: "workspace",
    });
    expect(selection.kind).toBe("ambiguous");
    expect(selection.kind === "ambiguous" && selection.candidates).toHaveLength(2);
  });

  it("reports absent for an empty or missing snapshot", () => {
    expect(selectSourceRoot(snapshot({ roots: [] }), { by: "workspace" })).toEqual({
      kind: "absent",
    });
    expect(selectSourceRoot(null, { by: "workspace" })).toEqual({ kind: "absent" });
  });

  it("resolves a lone worktree only through a run or token, never as the workspace", () => {
    const only = snapshot({ roots: [worktreeRoot()] });
    expect(selectSourceRoot(only, { by: "workspace" })).toEqual({ kind: "absent" });
    expect(selectSourceRoot(only, { by: "run", runId: "run-7" }).kind).toBe("resolved");
  });
});

describe("snapshot freshness", () => {
  it("knows when a snapshot is live", () => {
    expect(isSnapshotLive(snapshot(), 1_700_000_500_000)).toBe(true);
    expect(isSnapshotLive(snapshot(), 1_700_000_900_000)).toBe(false);
    expect(isSnapshotLive(null, 0)).toBe(false);
  });

  it("refreshes ahead of expiry rather than at it", () => {
    expect(shouldRefreshSnapshot(snapshot(), 1_700_000_500_000)).toBe(false);
    expect(shouldRefreshSnapshot(snapshot(), 1_700_000_880_000)).toBe(true);
    expect(shouldRefreshSnapshot(null, 0)).toBe(true);
  });
});

describe("appendSourceChunk", () => {
  const line = (number: number, text: string): SourceLine => ({ number, text, truncated: false });

  it("appends ordinary chunks", () => {
    const first = appendSourceChunk([], chunk({ lines: [line(1, "a"), line(2, "b")] }));
    const second = appendSourceChunk(
      first,
      chunk({ lines: [line(3, "c")], continuesPrevious: false }),
    );
    expect(second.map((entry) => entry.text)).toEqual(["a", "b", "c"]);
  });

  it("rejoins a line the previous chunk left unfinished", () => {
    const first = appendSourceChunk([], chunk({ lines: [line(1, "abcd")] }));
    const second = appendSourceChunk(
      first,
      chunk({ lines: [line(1, "efgh"), line(2, "second")], continuesPrevious: true }),
    );
    expect(second.map((entry) => [entry.number, entry.text])).toEqual([
      [1, "abcdefgh"],
      [2, "second"],
    ]);
  });

  it("does not rejoin when the numbers disagree", () => {
    const first = appendSourceChunk([], chunk({ lines: [line(1, "abcd")] }));
    const second = appendSourceChunk(
      first,
      chunk({ lines: [line(2, "efgh")], continuesPrevious: true }),
    );
    expect(second).toHaveLength(2);
  });

  it("carries a truncation flag through a rejoin", () => {
    const first = appendSourceChunk([], chunk({ lines: [{ number: 1, text: "ab", truncated: false }] }));
    const second = appendSourceChunk(
      first,
      chunk({ lines: [{ number: 1, text: "cd", truncated: true }], continuesPrevious: true }),
    );
    expect(second[0]).toEqual({ number: 1, text: "abcd", truncated: true });
  });

  it("leaves the input untouched", () => {
    const original = [line(1, "a")];
    appendSourceChunk(original, chunk({ lines: [line(2, "b")] }));
    expect(original).toEqual([line(1, "a")]);
  });
});

describe("refusals", () => {
  it("reads the code from a boundary refusal", () => {
    expect(parseSourceViewErrorCode(new Error("parent_escape: walks above the root"))).toBe(
      "parent_escape",
    );
    expect(parseSourceViewErrorCode("made_up_code: nope")).toBeNull();
    expect(parseSourceViewErrorCode(null)).toBeNull();
  });

  it("explains every published code without leaving one generic", () => {
    for (const code of SOURCE_VIEW_ERROR_CODES) {
      const summary = sourceViewErrorSummary(`${code}: detail`);
      expect(summary).not.toBe("The file could not be opened.");
      expect(summary.length).toBeGreaterThan(0);
    }
    expect(sourceViewErrorSummary("kernel panic")).toBe("The file could not be opened.");
  });

  it("classifies authorization refusals so a caller knows to re-snapshot", () => {
    for (const code of [
      "token_expired",
      "token_revoked",
      "policy_drift",
      "principal_mismatch",
      "snapshot_unknown",
    ]) {
      expect(isAuthorizationRefusal(`${code}: detail`)).toBe(true);
    }
    for (const code of ["parent_escape", "not_found", "range_invalid", "document_changed"]) {
      expect(isAuthorizationRefusal(`${code}: detail`)).toBe(false);
    }
  });
});

describe("display", () => {
  it("names the exact tree by kind, label, and digest, never by path", () => {
    expect(rootIdentityLabel(document())).toBe(
      `Workspace · repo/project · ${digestLabel(DIGEST_A)}`,
    );
    expect(rootIdentityLabel(document({ root: worktreeRoot() }))).toBe(
      `Isolated worktree · run run-7 · runs/run-7 · ${digestLabel(DIGEST_C)}`,
    );
  });

  it("says nothing when a projection is complete", () => {
    expect(projectionNotice(document())).toBeNull();
  });

  it("reports a prefix-only classification honestly", () => {
    expect(
      projectionNotice(
        document({ content: { verdict: "text", scannedBytes: 1_048_576, completeScan: false } }),
      ),
    ).toBe("classified from the first 1048576 bytes");
  });

  it("reports lossy decoding and pinned identity", () => {
    const notice = projectionNotice(
      document({
        content: { verdict: "text_lossy", scannedBytes: 10, completeScan: true },
        identity: { kind: "pinned", digest: DIGEST_A, stability: "heuristic" },
        chunk: chunk({ lossyReplacements: 2 }),
      }),
    );
    expect(notice).toMatch(/not valid UTF-8/);
    expect(notice).toMatch(/2 undecodable bytes/);
    expect(notice).toMatch(/a replaced file may not be detected/);
  });

  it("reports binary content without rendering it", () => {
    expect(
      projectionNotice(
        document({
          byteLen: 2048,
          content: { verdict: "binary", scannedBytes: 2048, completeScan: true },
          chunk: chunk({ lines: [], bytesConsumed: 0 }),
        }),
      ),
    ).toMatch(/binary content, 2048 bytes, not rendered as text/);
  });

  it("reports progress through a paged document", () => {
    expect(readProgress(document(), 1)).toBe("1 line · complete");
    const paged = document({
      byteLen: 1_000,
      chunk: chunk({
        eof: false,
        continuesNext: true,
        bytesConsumed: 250,
        nextCursor: {
          byteOffset: 250,
          nextLineNumber: 1,
          carryHex: "",
          continuesLine: true,
          documentDigest: DIGEST_A,
        },
      }),
    });
    expect(readProgress(paged, 40)).toBe("40 lines · 25% of 1000 bytes");
  });
});
