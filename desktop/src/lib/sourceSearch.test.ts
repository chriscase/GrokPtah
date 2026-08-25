import { describe, expect, it } from "vitest";
import {
  matchIndexAtOrAfter,
  rangePosition,
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

  it("honours case sensitivity and whole-word matching", () => {
    expect(searchLines(document, "Beta", { caseSensitive: true })).toEqual([
      { line: 2, start: 0, end: 4 },
    ]);
    expect(searchLines(document, "beta", { wholeWord: true })).toEqual([
      { line: 1, start: 6, end: 10 },
      { line: 2, start: 0, end: 4 },
    ]);
  });

  it("treats the needle as literal text, never as a pattern", () => {
    const source = lines("price is $5.00 (approx)", "a.c");
    expect(searchLines(source, "$5.00")).toEqual([{ line: 1, start: 9, end: 14 }]);
    expect(searchLines(source, ".*")).toEqual([]);
    expect(searchLines(source, "(approx)")).toEqual([{ line: 1, start: 15, end: 23 }]);
  });

  it("finds overlapping occurrences and stops at the limit", () => {
    expect(searchLines(lines("aaaa"), "aa")).toHaveLength(3);
    expect(searchLines(lines("a".repeat(100)), "a", { limit: 7 })).toHaveLength(7);
  });

  // --- Unicode -----------------------------------------------------------

  it("returns offsets that are code-point boundaries, never inside a surrogate pair", () => {
    const source = lines("🎯🎯needle🎯");
    const [match] = searchLines(source, "needle");
    // Two astral characters occupy four UTF-16 units.
    expect(match).toEqual({ line: 1, start: 4, end: 10 });
    expect(source[0].text.slice(match.start, match.end)).toBe("needle");
  });

  it("matches astral characters themselves without splitting them", () => {
    const source = lines("a🎯b🎯c");
    const matches = searchLines(source, "🎯");
    expect(matches).toHaveLength(2);
    for (const match of matches) {
      expect(source[0].text.slice(match.start, match.end)).toBe("🎯");
    }
  });

  it("keeps offsets correct when case folding changes length", () => {
    // "İ".toLowerCase() is two UTF-16 units, so a naive fold-then-index
    // implementation highlights the wrong characters from here on.
    const source = lines("İstanbul target");
    const [match] = searchLines(source, "target");
    expect(source[0].text.slice(match.start, match.end)).toBe("target");
  });

  it("matches across a length-changing fold without duplicating the hit", () => {
    const source = lines("Ißß");
    expect(searchLines(source, "ß")).toHaveLength(2);
  });

  it("finds a folded needle in text that differs only by case", () => {
    expect(searchLines(lines("STRASSE"), "strasse")).toEqual([{ line: 1, start: 0, end: 7 }]);
    expect(searchLines(lines("ÉCOLE"), "école")).toEqual([{ line: 1, start: 0, end: 5 }]);
  });

  it("treats non-ASCII letters as word characters for whole-word matching", () => {
    expect(searchLines(lines("café au lait"), "café", { wholeWord: true })).toHaveLength(1);
    expect(searchLines(lines("cafés"), "café", { wholeWord: true })).toHaveLength(0);
  });

  it("returns nothing for an empty query or bad input", () => {
    expect(searchLines(document, "")).toEqual([]);
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    expect(searchLines(null as any, "a")).toEqual([]);
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    expect(searchLines(document, undefined as any)).toEqual([]);
  });
});

describe("match navigation", () => {
  const matches = [
    { line: 3, start: 0, end: 1 },
    { line: 9, start: 0, end: 1 },
  ];

  it("finds the first match at or after a line, wrapping to the start", () => {
    expect(matchIndexAtOrAfter(matches, 1)).toBe(0);
    expect(matchIndexAtOrAfter(matches, 4)).toBe(1);
    expect(matchIndexAtOrAfter(matches, 100)).toBe(0);
    expect(matchIndexAtOrAfter([], 1)).toBe(-1);
  });

  it("steps with wrap-around and starts at an end when nothing is selected", () => {
    expect(stepMatch(3, 2, 1)).toBe(0);
    expect(stepMatch(3, 0, -1)).toBe(2);
    expect(stepMatch(3, -1, 1)).toBe(0);
    expect(stepMatch(3, -1, -1)).toBe(2);
    expect(stepMatch(0, 0, 1)).toBe(-1);
  });
});

describe("segmentLine", () => {
  it("splits a line into plain and matched runs", () => {
    expect(segmentLine("alpha beta", [{ line: 1, start: 6, end: 10 }])).toEqual([
      { text: "alpha ", matched: false, active: false },
      { text: "beta", matched: true, active: false },
    ]);
  });

  it("marks the active match so it reads differently from the rest", () => {
    const matches = [
      { line: 1, start: 0, end: 1 },
      { line: 1, start: 2, end: 3 },
    ];
    const segments = segmentLine("a b a", matches, matches[1]);
    expect(segments.filter((segment) => segment.active)).toHaveLength(1);
    expect(segments.find((segment) => segment.active)?.text).toBe("b");
  });

  it("is lossless across several and overlapping matches", () => {
    expect(
      segmentLine("abab", [
        { line: 1, start: 0, end: 2 },
        { line: 1, start: 2, end: 4 },
      ])
        .map((segment) => segment.text)
        .join(""),
    ).toBe("abab");
    expect(
      segmentLine("aaaa", [
        { line: 1, start: 0, end: 2 },
        { line: 1, start: 1, end: 3 },
      ])
        .map((segment) => segment.text)
        .join(""),
    ).toBe("aaaa");
  });

  it("keeps the whole line when nothing matched", () => {
    expect(segmentLine("plain", [])).toEqual([{ text: "plain", matched: false, active: false }]);
  });
});

describe("rangePosition", () => {
  it("places every line relative to a multi-line range", () => {
    const range = { firstLine: 10, lastLine: 13 };
    expect(rangePosition(range, 9)).toBe("outside");
    expect(rangePosition(range, 10)).toBe("first");
    expect(rangePosition(range, 11)).toBe("middle");
    expect(rangePosition(range, 13)).toBe("last");
    expect(rangePosition(range, 14)).toBe("outside");
  });

  it("marks a single-line range as its own shape", () => {
    expect(rangePosition({ firstLine: 5, lastLine: 5 }, 5)).toBe("only");
  });

  it("tolerates an inverted range", () => {
    expect(rangePosition({ firstLine: 13, lastLine: 10 }, 11)).toBe("middle");
  });

  it("places nothing when there is no range", () => {
    expect(rangePosition(null, 1)).toBe("outside");
  });
});

describe("searchStatus", () => {
  it("announces position, emptiness, and idleness", () => {
    expect(searchStatus(4, 1, "beta")).toBe("Match 2 of 4 for beta");
    expect(searchStatus(0, -1, "beta")).toBe("No matches for beta");
    expect(searchStatus(0, -1, "")).toBe("");
  });
});
