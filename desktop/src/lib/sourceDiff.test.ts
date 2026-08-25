import { describe, expect, it } from "vitest";
import {
  changeSummary,
  diffTotals,
  firstChangedLine,
  isOpenable,
  parseUnifiedDiff,
  readDiffEvidence,
} from "./sourceDiff";

const MODIFIED = [
  "diff --git a/src/lib/api.ts b/src/lib/api.ts",
  "index 1111111..2222222 100644",
  "--- a/src/lib/api.ts",
  "+++ b/src/lib/api.ts",
  "@@ -10,6 +10,7 @@ export const api = {",
  "   fileTree: () => invoke('file_tree'),",
  "-  legacy: () => invoke('legacy'),",
  "+  sourceViewRoots: () => invoke('source_view_roots'),",
  "+  sourceViewOpen: () => invoke('source_view_open'),",
  "   gitStatus: () => invoke('git_status'),",
  "",
].join("\n");

describe("parseUnifiedDiff", () => {
  it("reads paths, counts, and hunk ranges", () => {
    const [file] = parseUnifiedDiff(MODIFIED);
    expect(file.path).toBe("src/lib/api.ts");
    expect(file.oldPath).toBe("src/lib/api.ts");
    expect(file.newPath).toBe("src/lib/api.ts");
    expect(file.status).toBe("modified");
    expect(file.additions).toBe(2);
    expect(file.deletions).toBe(1);
    expect(file.hunks).toHaveLength(1);
    expect(file.hunks[0]).toMatchObject({
      oldStart: 10,
      oldLines: 6,
      newStart: 10,
      newLines: 7,
      heading: "export const api = {",
    });
  });

  it("numbers every line on the side it exists", () => {
    const [file] = parseUnifiedDiff(MODIFIED);
    expect(file.hunks[0].lines.map((l) => [l.kind, l.oldNumber, l.newNumber])).toEqual([
      ["context", 10, 10],
      ["remove", 11, null],
      ["add", null, 11],
      ["add", null, 12],
      ["context", 12, 13],
    ]);
  });

  it("reads an added file", () => {
    const diff = [
      "diff --git a/src/new.ts b/src/new.ts",
      "new file mode 100644",
      "--- /dev/null",
      "+++ b/src/new.ts",
      "@@ -0,0 +1,2 @@",
      "+export const a = 1;",
      "+export const b = 2;",
    ].join("\n");
    const [file] = parseUnifiedDiff(diff);
    expect(file.status).toBe("added");
    expect(file.oldPath).toBeNull();
    expect(file.path).toBe("src/new.ts");
    expect(file.additions).toBe(2);
    expect(firstChangedLine(file)).toBe(1);
  });

  it("reads a removed file and keeps its old path openable-free", () => {
    const diff = [
      "diff --git a/src/gone.ts b/src/gone.ts",
      "deleted file mode 100644",
      "--- a/src/gone.ts",
      "+++ /dev/null",
      "@@ -1,2 +0,0 @@",
      "-export const a = 1;",
      "-export const b = 2;",
    ].join("\n");
    const [file] = parseUnifiedDiff(diff);
    expect(file.status).toBe("removed");
    expect(file.newPath).toBeNull();
    expect(file.path).toBe("src/gone.ts");
    expect(file.deletions).toBe(2);
    expect(isOpenable(file)).toBe(false);
  });

  it("reads a rename", () => {
    const diff = [
      "diff --git a/src/old.ts b/src/new.ts",
      "similarity index 98%",
      "rename from src/old.ts",
      "rename to src/new.ts",
      "--- a/src/old.ts",
      "+++ b/src/new.ts",
      "@@ -1 +1 @@",
      "-const a = 1;",
      "+const a = 2;",
    ].join("\n");
    const [file] = parseUnifiedDiff(diff);
    expect(file.status).toBe("renamed");
    expect(file.oldPath).toBe("src/old.ts");
    expect(file.newPath).toBe("src/new.ts");
    expect(file.path).toBe("src/new.ts");
  });

  it("flags a binary file and refuses to offer it for opening", () => {
    const diff = [
      "diff --git a/assets/icon.png b/assets/icon.png",
      "index 3333333..4444444 100644",
      "Binary files a/assets/icon.png and b/assets/icon.png differ",
    ].join("\n");
    const [file] = parseUnifiedDiff(diff);
    expect(file.binary).toBe(true);
    expect(file.path).toBe("assets/icon.png");
    expect(isOpenable(file)).toBe(false);
    expect(changeSummary(file)).toBe("binary");
  });

  it("separates several files in one diff", () => {
    const diff = `${MODIFIED}\n${[
      "diff --git a/src/other.ts b/src/other.ts",
      "--- a/src/other.ts",
      "+++ b/src/other.ts",
      "@@ -1 +1 @@",
      "-a",
      "+b",
    ].join("\n")}`;
    const files = parseUnifiedDiff(diff);
    expect(files.map((f) => f.path)).toEqual(["src/lib/api.ts", "src/other.ts"]);
    expect(diffTotals(files)).toEqual({ files: 2, additions: 3, deletions: 2 });
  });

  it("keeps complete files from a truncated diff", () => {
    const truncated = `${MODIFIED}\ndiff --git a/src/cut.ts b/src/cut.ts\n--- a/src/cut.ts\n+++ b/src/cut.ts\n@@ -1,4 +1,4 @@\n-a`;
    const files = parseUnifiedDiff(truncated);
    expect(files).toHaveLength(2);
    expect(files[0].additions).toBe(2);
    expect(files[1].deletions).toBe(1);
  });

  it("handles a single-line hunk header with no counts", () => {
    const diff = ["--- a/x.ts", "+++ b/x.ts", "@@ -3 +3 @@", "-a", "+b"].join("\n");
    const [file] = parseUnifiedDiff(diff);
    expect(file.hunks[0]).toMatchObject({ oldStart: 3, oldLines: 1, newStart: 3, newLines: 1 });
    expect(firstChangedLine(file)).toBe(3);
  });

  it("ignores the no-newline marker without shifting numbers", () => {
    const diff = [
      "--- a/x.ts",
      "+++ b/x.ts",
      "@@ -1,2 +1,2 @@",
      " first",
      "-second",
      "\\ No newline at end of file",
      "+second!",
      "\\ No newline at end of file",
    ].join("\n");
    const [file] = parseUnifiedDiff(diff);
    expect(file.hunks[0].lines.map((l) => l.kind)).toEqual(["context", "remove", "add"]);
    expect(file.hunks[0].lines[2].newNumber).toBe(2);
  });

  it("treats an empty diff line as unchanged context", () => {
    const diff = ["--- a/x.ts", "+++ b/x.ts", "@@ -1,3 +1,3 @@", " a", "", "+c", "-b"].join("\n");
    const [file] = parseUnifiedDiff(diff);
    expect(file.hunks[0].lines[1]).toEqual({
      kind: "context",
      text: "",
      oldNumber: 2,
      newNumber: 2,
    });
  });

  it("tolerates CRLF diff text", () => {
    const [file] = parseUnifiedDiff(MODIFIED.split("\n").join("\r\n"));
    expect(file.path).toBe("src/lib/api.ts");
    expect(file.additions).toBe(2);
  });

  it("returns nothing for empty or non-diff input", () => {
    expect(parseUnifiedDiff("")).toEqual([]);
    expect(parseUnifiedDiff("   ")).toEqual([]);
    expect(parseUnifiedDiff("no changes here")).toEqual([]);
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    expect(parseUnifiedDiff(null as any)).toEqual([]);
  });

  it("does not mistake diff content for a file header", () => {
    const diff = [
      "--- a/doc.md",
      "+++ b/doc.md",
      "@@ -1,2 +1,3 @@",
      " intro",
      "+diff --git a/fake.ts b/fake.ts",
    ].join("\n");
    const files = parseUnifiedDiff(diff);
    expect(files).toHaveLength(1);
    expect(files[0].path).toBe("doc.md");
    expect(files[0].hunks[0].lines[1].text).toBe("diff --git a/fake.ts b/fake.ts");
  });
});

describe("firstChangedLine", () => {
  it("prefers the first added line", () => {
    const [file] = parseUnifiedDiff(MODIFIED);
    expect(firstChangedLine(file)).toBe(11);
  });

  it("falls back to the hunk start for a delete-only change", () => {
    const diff = ["--- a/x.ts", "+++ b/x.ts", "@@ -5,2 +5,1 @@", " keep", "-drop"].join("\n");
    const [file] = parseUnifiedDiff(diff);
    expect(firstChangedLine(file)).toBe(5);
  });

  it("returns null when a file has no hunks", () => {
    const diff = [
      "diff --git a/assets/icon.png b/assets/icon.png",
      "Binary files a/assets/icon.png and b/assets/icon.png differ",
    ].join("\n");
    const [file] = parseUnifiedDiff(diff);
    expect(firstChangedLine(file)).toBeNull();
  });
});

describe("changeSummary", () => {
  it("renders counts with a real minus sign", () => {
    const [file] = parseUnifiedDiff(MODIFIED);
    expect(changeSummary(file)).toBe("+2 −1");
  });
});

describe("readDiffEvidence", () => {
  it("reports complete evidence for a whole, fully parsed diff", () => {
    const evidence = readDiffEvidence(MODIFIED, false);
    expect(evidence.complete).toBe(true);
    expect(evidence.reasons).toEqual([]);
    expect(evidence.unrecognizedLines).toBe(0);
    expect(evidence.files).toHaveLength(1);
    expect(evidence.raw).toBe(MODIFIED);
  });

  it("is incomplete when the review capped the diff, however well it parsed", () => {
    const evidence = readDiffEvidence(MODIFIED, true);
    expect(evidence.complete).toBe(false);
    expect(evidence.reasons[0]).toMatch(/capped the diff/);
    expect(evidence.files).toHaveLength(1);
  });

  it("is incomplete and carries the raw text when nothing parses", () => {
    const evidence = readDiffEvidence("this is not a diff at all\nsecond line\n", false);
    expect(evidence.complete).toBe(false);
    expect(evidence.files).toEqual([]);
    expect(evidence.reasons).toContain("no file could be read from the diff");
    expect(evidence.raw).toContain("this is not a diff at all");
  });

  it("counts lines it could not attribute to a file", () => {
    const evidence = readDiffEvidence(
      ["--- a/x.ts", "+++ b/x.ts", "@@ -1 +1 @@", "-a", "+b", "?? stray marker"].join("\n"),
      false,
    );
    expect(evidence.unrecognizedLines).toBe(1);
    expect(evidence.complete).toBe(false);
    expect(evidence.reasons.join(" ")).toMatch(/could not be attributed/);
  });

  it("does not count ordinary Git metadata as unattributed", () => {
    const evidence = readDiffEvidence(
      [
        "diff --git a/src/new.ts b/src/new.ts",
        "new file mode 100644",
        "index 0000000..1111111",
        "--- /dev/null",
        "+++ b/src/new.ts",
        "@@ -0,0 +1 @@",
        "+export const a = 1;",
      ].join("\n"),
      false,
    );
    expect(evidence.unrecognizedLines).toBe(0);
    expect(evidence.complete).toBe(true);
  });

  it("treats an empty diff as incomplete evidence rather than complete emptiness", () => {
    const evidence = readDiffEvidence("", false);
    expect(evidence.complete).toBe(false);
    expect(evidence.files).toEqual([]);
  });

  it("tolerates a non-string diff without throwing", () => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const evidence = readDiffEvidence(null as any, false);
    expect(evidence.raw).toBe("");
    expect(evidence.complete).toBe(false);
  });
});
