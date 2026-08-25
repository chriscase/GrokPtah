import { describe, expect, it } from "vitest";
import {
  SOURCE_VIEW_ERROR_CODES,
  parseSourceDocument,
  parseSourceRoot,
  parseSourceRoots,
  parseSourceViewErrorCode,
  pickSourceRoot,
  rootIdentityLabel,
  sourceViewErrorSummary,
  truncationNotice,
  type SourceDocument,
} from "./sourceView";

const ROOT = {
  id: "ws-0123456789abcdef",
  kind: "workspace",
  label: "repo/project",
  path: "/approved/repo/project",
  runId: null,
};

const DOCUMENT: Record<string, unknown> = {
  rootId: ROOT.id,
  rootKind: "workspace",
  rootPath: ROOT.path,
  rootLabel: ROOT.label,
  runId: null,
  relativePath: "src/main.rs",
  absolutePath: "/approved/repo/project/src/main.rs",
  language: "rust",
  encoding: "utf8",
  byteLen: 13,
  bytesRead: 13,
  lines: [{ number: 1, text: "fn main() {}", truncated: false }],
  lineCount: 1,
  truncatedBytes: false,
  truncatedLines: false,
  lossyReplacements: 0,
  eol: "lf",
  contentFingerprint: "fnv1a64:0123456789abcdef",
};

function doc(overrides: Record<string, unknown> = {}): SourceDocument {
  return parseSourceDocument({ ...DOCUMENT, ...overrides });
}

describe("parseSourceRoot", () => {
  it("accepts a well-formed root", () => {
    expect(parseSourceRoot(ROOT)).toEqual(ROOT);
  });

  it("accepts an isolated worktree with its run", () => {
    const worktree = { ...ROOT, kind: "isolated_worktree", runId: "run-9" };
    expect(parseSourceRoot(worktree).runId).toBe("run-9");
  });

  it("treats a missing runId as absent rather than failing", () => {
    const { runId: _runId, ...withoutRun } = ROOT;
    expect(parseSourceRoot(withoutRun).runId).toBeNull();
  });

  it("refuses an unknown kind", () => {
    expect(() => parseSourceRoot({ ...ROOT, kind: "anywhere" })).toThrow(/kind must be one of/);
  });

  it("refuses non-objects and missing fields", () => {
    expect(() => parseSourceRoot(null)).toThrow(/must be an object/);
    expect(() => parseSourceRoot([ROOT])).toThrow(/must be an object/);
    expect(() => parseSourceRoot({ ...ROOT, path: 7 })).toThrow(/path must be a string/);
  });
});

describe("parseSourceRoots", () => {
  it("accepts a list", () => {
    expect(parseSourceRoots([ROOT])).toHaveLength(1);
  });

  it("refuses the whole list when one entry is malformed", () => {
    expect(() => parseSourceRoots([ROOT, { ...ROOT, id: 1 }])).toThrow(/id must be a string/);
  });

  it("refuses a non-array", () => {
    expect(() => parseSourceRoots({})).toThrow(/must be an array/);
  });
});

describe("parseSourceDocument", () => {
  it("accepts a well-formed document", () => {
    expect(doc().lines[0].text).toBe("fn main() {}");
    expect(doc().relativePath).toBe("src/main.rs");
  });

  it("refuses an unknown encoding or line ending", () => {
    expect(() => doc({ encoding: "utf16" })).toThrow(/encoding must be one of/);
    expect(() => doc({ eol: "cr" })).toThrow(/eol must be one of/);
  });

  it("refuses a binary document that still carries text", () => {
    expect(() => doc({ encoding: "binary" })).toThrow(/must not carry rendered lines/);
  });

  it("accepts a binary document with no lines", () => {
    const binary = doc({ encoding: "binary", lines: [], lineCount: 0 });
    expect(binary.encoding).toBe("binary");
    expect(binary.lines).toEqual([]);
  });

  it("refuses a zero or fractional line number", () => {
    expect(() => doc({ lines: [{ number: 0, text: "x", truncated: false }] })).toThrow(
      /1-based integer/,
    );
    expect(() => doc({ lines: [{ number: 1.5, text: "x", truncated: false }] })).toThrow(
      /1-based integer/,
    );
  });

  it("refuses negative counts", () => {
    expect(() => doc({ byteLen: -1 })).toThrow(/non-negative/);
    expect(() => doc({ lossyReplacements: -2 })).toThrow(/non-negative/);
  });

  it("refuses a non-boolean truncation flag", () => {
    expect(() => doc({ truncatedBytes: "yes" })).toThrow(/must be a boolean/);
  });

  it("refuses a non-array lines field", () => {
    expect(() => doc({ lines: "text" })).toThrow(/lines must be an array/);
  });
});

describe("parseSourceViewErrorCode", () => {
  it("reads the code from a boundary refusal", () => {
    expect(parseSourceViewErrorCode(new Error("parent_escape: walks above the root"))).toBe(
      "parent_escape",
    );
    expect(parseSourceViewErrorCode("symlink_rejected: `link` is a symbolic link")).toBe(
      "symlink_rejected",
    );
  });

  it("returns null for anything it does not recognise", () => {
    expect(parseSourceViewErrorCode("boom")).toBeNull();
    expect(parseSourceViewErrorCode("made_up_code: nope")).toBeNull();
    expect(parseSourceViewErrorCode(null)).toBeNull();
    expect(parseSourceViewErrorCode(": leading colon")).toBeNull();
  });

  it("covers every published code", () => {
    for (const code of SOURCE_VIEW_ERROR_CODES) {
      expect(parseSourceViewErrorCode(`${code}: detail`)).toBe(code);
      expect(sourceViewErrorSummary(`${code}: detail`)).not.toBe("");
    }
  });
});

describe("sourceViewErrorSummary", () => {
  it("explains containment refusals without blaming the reader", () => {
    expect(sourceViewErrorSummary("parent_escape: x")).toMatch(/outside the approved workspace/);
    expect(sourceViewErrorSummary("symlink_rejected: x")).toMatch(/never followed/);
    expect(sourceViewErrorSummary("too_large: x")).toMatch(/larger than/);
  });

  it("falls back to a plain sentence for an unknown failure", () => {
    expect(sourceViewErrorSummary("kernel panic")).toBe("The file could not be opened.");
  });
});

describe("truncationNotice", () => {
  it("is null for a complete document", () => {
    expect(truncationNotice(doc())).toBeNull();
  });

  it("reports byte truncation with both numbers", () => {
    expect(truncationNotice(doc({ truncatedBytes: true, bytesRead: 100, byteLen: 900 }))).toBe(
      "showing the first 100 of 900 bytes",
    );
  });

  it("reports line truncation with both numbers", () => {
    expect(truncationNotice(doc({ truncatedLines: true, lineCount: 40 }))).toBe(
      "showing the first 1 of 40 lines",
    );
  });

  it("reports lossy decoding, singular and plural", () => {
    expect(truncationNotice(doc({ encoding: "utf8_lossy", lossyReplacements: 1 }))).toMatch(
      /1 byte could not be decoded/,
    );
    expect(truncationNotice(doc({ encoding: "utf8_lossy", lossyReplacements: 3 }))).toMatch(
      /3 bytes could not be decoded/,
    );
  });

  it("joins several notices", () => {
    const notice = truncationNotice(
      doc({ truncatedBytes: true, truncatedLines: true, bytesRead: 5, byteLen: 50, lineCount: 9 }),
    );
    expect(notice).toBe("showing the first 5 of 50 bytes · showing the first 1 of 9 lines");
  });
});

describe("rootIdentityLabel", () => {
  it("names the workspace and its exact path", () => {
    expect(rootIdentityLabel(doc())).toBe("Workspace · /approved/repo/project");
  });

  it("names an isolated worktree and its run", () => {
    const worktree = doc({
      rootKind: "isolated_worktree",
      runId: "run-9",
      rootPath: "/approved/repo/project/.grokptah/worktrees/runs/run-9",
    });
    expect(rootIdentityLabel(worktree)).toBe(
      "Isolated worktree · run run-9 · /approved/repo/project/.grokptah/worktrees/runs/run-9",
    );
  });
});

describe("pickSourceRoot", () => {
  const workspace = { ...ROOT };
  const worktree = {
    id: "wt-aaaaaaaaaaaaaaaa",
    kind: "isolated_worktree" as const,
    label: "run run-7 worktree",
    path: "/approved/repo/project/.grokptah/worktrees/runs/run-7",
    runId: "run-7",
  };
  const roots = [workspace, worktree];

  it("defaults to the shared workspace", () => {
    expect(pickSourceRoot(roots)).toBe(workspace);
  });

  it("honours an exact root id", () => {
    expect(pickSourceRoot(roots, { rootId: worktree.id })).toBe(worktree);
  });

  it("returns null for a root id that is no longer approved", () => {
    expect(pickSourceRoot(roots, { rootId: "ws-deadbeefdeadbeef" })).toBeNull();
  });

  it("prefers a run's own worktree when a run is named", () => {
    expect(pickSourceRoot(roots, { runId: "run-7" })).toBe(worktree);
  });

  it("refuses rather than falling back to the workspace for an unknown run", () => {
    expect(pickSourceRoot(roots, { runId: "run-absent" })).toBeNull();
  });

  it("falls back to the first root when none is a workspace", () => {
    expect(pickSourceRoot([worktree])).toBe(worktree);
  });

  it("returns null when nothing is approved", () => {
    expect(pickSourceRoot([])).toBeNull();
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    expect(pickSourceRoot(null as any)).toBeNull();
  });
});
