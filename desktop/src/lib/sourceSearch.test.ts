import { describe, expect, it } from "vitest";
import {
  matchIndexAtOrAfter,
  searchLines,
  searchStatus,
  segmentLine,
  stepMatch,
} from "./sourceSearch";
import type { SourceLine } from "./sourceView";

function lines(...texts: string[]): SourceLine[] {
  return texts.map((text, index) => ({ number: index + 1, text, truncated: false }));
}

describe("searchLines", () => {
  const document = lines("alpha beta", "Beta gamma", "betabeta", "");

  it("is case-insensitive by default and reports real line numbers", () => {
    expect(searchLines(document, "beta")).toEqual([
      { line: 1, start: 6, end: 10 },
      { line: 2, start: 0, end: 4 },
      { line: 3, start: 0, end: 4 },
      { line: 3, start: 4, end: 8 },
    ]);
  });

  it("honours case sensitivity", () => {
    expect(searchLines(document, "Beta", { caseSensitive: true })).toEqual([
      { line: 2, start: 0, end: 4 },
    ]);
  });

  it("honours whole-word matching", () => {
    expect(searchLines(document, "beta", { wholeWord: true })).toEqual([
      { line: 1, start: 6, end: 10 },
      { line: 2, start: 0, end: 4 },
    ]);
  });

  it("finds overlapping occurrences", () => {
    expect(searchLines(lines("aaaa"), "aa")).toHaveLength(3);
  });

  it("treats the needle as literal text, never as a pattern", () => {
    const source = lines("price is $5.00 (approx)", "a.c");
    expect(searchLines(source, "$5.00")).toEqual([{ line: 1, start: 9, end: 14 }]);
    expect(searchLines(source, "a.c")).toEqual([{ line: 2, start: 0, end: 3 }]);
    expect(searchLines(source, ".*")).toEqual([]);
  });

  it("stops at the limit", () => {
    expect(searchLines(lines("a".repeat(100)), "a", { limit: 7 })).toHaveLength(7);
  });

  it("returns nothing for an empty query or bad input", () => {
    expect(searchLines(document, "")).toEqual([]);
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    expect(searchLines(null as any, "a")).toEqual([]);
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    expect(searchLines(document, undefined as any)).toEqual([]);
  });
});

describe("matchIndexAtOrAfter", () => {
  const matches = [
    { line: 3, start: 0, end: 1 },
    { line: 9, start: 0, end: 1 },
  ];

  it("finds the first match at or after a line", () => {
    expect(matchIndexAtOrAfter(matches, 1)).toBe(0);
    expect(matchIndexAtOrAfter(matches, 3)).toBe(0);
    expect(matchIndexAtOrAfter(matches, 4)).toBe(1);
  });

  it("wraps to the start when nothing follows", () => {
    expect(matchIndexAtOrAfter(matches, 100)).toBe(0);
  });

  it("reports -1 when there is nothing to find", () => {
    expect(matchIndexAtOrAfter([], 1)).toBe(-1);
  });
});

describe("stepMatch", () => {
  it("wraps forward and backward", () => {
    expect(stepMatch(3, 2, 1)).toBe(0);
    expect(stepMatch(3, 0, -1)).toBe(2);
    expect(stepMatch(3, 1, 1)).toBe(2);
  });

  it("starts at an end when nothing is selected", () => {
    expect(stepMatch(3, -1, 1)).toBe(0);
    expect(stepMatch(3, -1, -1)).toBe(2);
  });

  it("reports -1 with no matches", () => {
    expect(stepMatch(0, 0, 1)).toBe(-1);
  });
});

describe("segmentLine", () => {
  it("splits a line into plain and matched runs", () => {
    expect(segmentLine("alpha beta", [{ line: 1, start: 6, end: 10 }])).toEqual([
      { text: "alpha ", matched: false },
      { text: "beta", matched: true },
    ]);
  });

  it("handles a match at the very start", () => {
    expect(segmentLine("beta!", [{ line: 1, start: 0, end: 4 }])).toEqual([
      { text: "beta", matched: true },
      { text: "!", matched: false },
    ]);
  });

  it("keeps the whole line when nothing matched", () => {
    expect(segmentLine("plain", [])).toEqual([{ text: "plain", matched: false }]);
  });

  it("is lossless across several matches", () => {
    const text = "abab";
    const segments = segmentLine(text, [
      { line: 1, start: 0, end: 2 },
      { line: 1, start: 2, end: 4 },
    ]);
    expect(segments.map((s) => s.text).join("")).toBe(text);
  });

  it("drops an overlapping match rather than duplicating text", () => {
    const text = "aaaa";
    const segments = segmentLine(text, [
      { line: 1, start: 0, end: 2 },
      { line: 1, start: 1, end: 3 },
    ]);
    expect(segments.map((s) => s.text).join("")).toBe(text);
  });
});

describe("searchStatus", () => {
  it("announces position, emptiness, and idleness", () => {
    expect(searchStatus(4, 1, "beta")).toBe("Match 2 of 4 for beta");
    expect(searchStatus(0, -1, "beta")).toBe("No matches for beta");
    expect(searchStatus(0, -1, "")).toBe("");
  });
});
