import { describe, expect, it } from "vitest";
import {
  findSourceLocators,
  formatSourceLocator,
  looksLikeFilePath,
  parseSourceLocator,
  stripDiffPathPrefix,
} from "./sourceLocator";

describe("parseSourceLocator", () => {
  it("reads a bare path with no position", () => {
    expect(parseSourceLocator("src/lib/api.ts")).toEqual({
      path: "src/lib/api.ts",
      line: null,
      column: null,
    });
  });

  it("reads path:line and path:line:column", () => {
    expect(parseSourceLocator("src/lib/api.ts:42")).toEqual({
      path: "src/lib/api.ts",
      line: 42,
      column: null,
    });
    expect(parseSourceLocator("src/lib/api.ts:42:7")).toEqual({
      path: "src/lib/api.ts",
      line: 42,
      column: 7,
    });
  });

  it("stops after two positions so a third colon stays in the path", () => {
    expect(parseSourceLocator("weird:name.ts:10:2")).toEqual({
      path: "weird:name.ts",
      line: 10,
      column: 2,
    });
  });

  it("reads the compiler parenthesis form", () => {
    expect(parseSourceLocator("src/app.tsx(120,4)")).toEqual({
      path: "src/app.tsx",
      line: 120,
      column: 4,
    });
    expect(parseSourceLocator("src/app.tsx(120)")).toEqual({
      path: "src/app.tsx",
      line: 120,
      column: null,
    });
  });

  it("reads the spelled-out runtime form", () => {
    expect(parseSourceLocator("scripts/run.py, line 9")).toEqual({
      path: "scripts/run.py",
      line: 9,
      column: null,
    });
    expect(parseSourceLocator("scripts/run.py line 9")).toEqual({
      path: "scripts/run.py",
      line: 9,
      column: null,
    });
    expect(parseSourceLocator("scripts/run.py, line 9, column 4")).toEqual({
      path: "scripts/run.py",
      line: 9,
      column: 4,
    });
  });

  it("never reads a Windows drive colon as a line number", () => {
    expect(parseSourceLocator("C:\\repo\\src\\main.rs:12")).toEqual({
      path: "C:\\repo\\src\\main.rs",
      line: 12,
      column: null,
    });
    expect(parseSourceLocator("C:\\repo\\src\\main.rs")).toEqual({
      path: "C:\\repo\\src\\main.rs",
      line: null,
      column: null,
    });
    expect(parseSourceLocator("C:")).toBeNull();
    expect(parseSourceLocator("C:\\")).toBeNull();
  });

  it("peels quotes, backticks, and brackets", () => {
    for (const wrapped of [
      "'src/a.rs:3'",
      '"src/a.rs:3"',
      "`src/a.rs:3`",
      "(src/a.rs:3)",
      "[src/a.rs:3]",
      "<src/a.rs:3>",
    ]) {
      expect(parseSourceLocator(wrapped)).toEqual({ path: "src/a.rs", line: 3, column: null });
    }
  });

  it("drops trailing punctuation that carries no meaning", () => {
    expect(parseSourceLocator("src/a.rs:3:")).toEqual({ path: "src/a.rs", line: 3, column: null });
    expect(parseSourceLocator("src/a.rs:3,")).toEqual({ path: "src/a.rs", line: 3, column: null });
  });

  it("keeps a real extension that looks like trailing punctuation", () => {
    expect(parseSourceLocator("CHANGELOG.md")).toEqual({
      path: "CHANGELOG.md",
      line: null,
      column: null,
    });
  });

  it("refuses tokens that are not files", () => {
    expect(parseSourceLocator("")).toBeNull();
    expect(parseSourceLocator("   ")).toBeNull();
    expect(parseSourceLocator("42")).toBeNull();
    expect(parseSourceLocator("a\0b")).toBeNull();
    expect(parseSourceLocator("a\nb")).toBeNull();
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    expect(parseSourceLocator(undefined as any)).toBeNull();
  });

  it("refuses a zero or absurd position rather than inventing line 0", () => {
    expect(parseSourceLocator("src/a.rs:0")).toEqual({
      path: "src/a.rs:0",
      line: null,
      column: null,
    });
    expect(parseSourceLocator("src/a.rs:99999999999")).toEqual({
      path: "src/a.rs:99999999999",
      line: null,
      column: null,
    });
  });

  it("passes an escape attempt through untouched for the boundary to refuse", () => {
    expect(parseSourceLocator("../../etc/passwd:1")).toEqual({
      path: "../../etc/passwd",
      line: 1,
      column: null,
    });
  });

  it("drops a column when there is no line to anchor it to", () => {
    expect(parseSourceLocator("src/a.rs")?.column).toBeNull();
  });
});

describe("looksLikeFilePath", () => {
  it("accepts extensions, separators, and well-known bare names", () => {
    expect(looksLikeFilePath("src/a.rs")).toBe(true);
    expect(looksLikeFilePath("a.rs")).toBe(true);
    expect(looksLikeFilePath("src/nested/thing")).toBe(true);
    expect(looksLikeFilePath("Dockerfile")).toBe(true);
    expect(looksLikeFilePath("Cargo.lock")).toBe(true);
  });

  it("rejects prose and empty input", () => {
    expect(looksLikeFilePath("")).toBe(false);
    expect(looksLikeFilePath("passed")).toBe(false);
    expect(looksLikeFilePath("a  b")).toBe(false);
    expect(looksLikeFilePath("x".repeat(5000))).toBe(false);
  });
});

describe("findSourceLocators", () => {
  it("finds references in compiler output", () => {
    const text = [
      "error[E0382]: borrow of moved value",
      "  --> crates/common/thing/src/lib.rs:164:59",
      "   |",
      "note: also see src/app.tsx(12,4)",
    ].join("\n");
    expect(findSourceLocators(text)).toEqual([
      { path: "crates/common/thing/src/lib.rs", line: 164, column: 59 },
      { path: "src/app.tsx", line: 12, column: 4 },
    ]);
  });

  it("finds references in test output", () => {
    const text = "FAIL src/lib/promptQueue.test.ts > keeps order\n  at src/lib/promptQueue.ts:88:11";
    expect(findSourceLocators(text)).toEqual([
      { path: "src/lib/promptQueue.test.ts", line: null, column: null },
      { path: "src/lib/promptQueue.ts", line: 88, column: 11 },
    ]);
  });

  it("de-duplicates repeats but keeps distinct positions", () => {
    const text = "a/b.ts:1 a/b.ts:1 a/b.ts:2";
    expect(findSourceLocators(text)).toEqual([
      { path: "a/b.ts", line: 1, column: null },
      { path: "a/b.ts", line: 2, column: null },
    ]);
  });

  it("honours the result limit", () => {
    const text = Array.from({ length: 50 }, (_, i) => `src/f${i}.ts:1`).join(" ");
    expect(findSourceLocators(text, 5)).toHaveLength(5);
  });

  it("returns nothing for prose or non-strings", () => {
    expect(findSourceLocators("all tests passed in 4 seconds")).toEqual([]);
    expect(findSourceLocators("")).toEqual([]);
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    expect(findSourceLocators(null as any)).toEqual([]);
  });
});

describe("stripDiffPathPrefix", () => {
  it("removes only Git's diff-side prefixes", () => {
    expect(stripDiffPathPrefix("a/src/main.rs")).toBe("src/main.rs");
    expect(stripDiffPathPrefix("b/src/main.rs")).toBe("src/main.rs");
    expect(stripDiffPathPrefix("src/main.rs")).toBe("src/main.rs");
    expect(stripDiffPathPrefix("ab/src/main.rs")).toBe("ab/src/main.rs");
  });
});

describe("formatSourceLocator", () => {
  it("round-trips the shapes it produces", () => {
    for (const raw of ["src/a.rs", "src/a.rs:9", "src/a.rs:9:2"]) {
      const parsed = parseSourceLocator(raw);
      expect(parsed).not.toBeNull();
      expect(formatSourceLocator(parsed!)).toBe(raw);
    }
  });
});
