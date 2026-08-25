/**
 * In-file search over loaded lines.
 *
 * Two things this gets right that a naive implementation does not:
 *
 * * **Matching is literal, never a pattern.** A find box must not be able to
 *   hang the viewer on a pathological regular expression.
 * * **Case folding does not corrupt offsets.** `"İ".toLowerCase()` is two
 *   UTF-16 units, so folding a whole line and then using the match offset
 *   against the *original* line silently highlights the wrong characters.
 *   Folding here is per code point with an index map back to the original, and
 *   every returned offset is a code-point boundary.
 */

import type { SourceLine } from "./sourceView";

/** One match, as a half-open range of UTF-16 offsets in the original text. */
export interface SourceMatch {
  line: number;
  start: number;
  end: number;
}

export interface SourceSearchOptions {
  caseSensitive?: boolean;
  wholeWord?: boolean;
  /** Stop after this many matches so a broad query stays responsive. */
  limit?: number;
}

const DEFAULT_LIMIT = 5_000;
/** Word characters for whole-word matching, including non-ASCII letters. */
const WORD_CHARACTER = /[\p{L}\p{N}_]/u;

/**
 * Case-fold `text` code point by code point, keeping a map from each folded
 * UTF-16 offset back to the offset of the source code point that produced it.
 *
 * The map has one extra trailing entry, so a match end that lands at the end
 * of the string maps to the end of the original.
 */
function foldWithMap(text: string): { folded: string; map: number[] } {
  let folded = "";
  const map: number[] = [];
  for (let index = 0; index < text.length; ) {
    const codePoint = text.codePointAt(index) as number;
    const source = String.fromCodePoint(codePoint);
    const lowered = source.toLowerCase();
    for (let unit = 0; unit < lowered.length; unit += 1) map.push(index);
    folded += lowered;
    index += source.length;
  }
  map.push(text.length);
  return { folded, map };
}

/** Step forward one code point from `index`. */
function nextBoundary(text: string, index: number): number {
  if (index >= text.length) return text.length;
  const codePoint = text.codePointAt(index) as number;
  return index + String.fromCodePoint(codePoint).length;
}

function isWordBoundary(text: string, start: number, end: number): boolean {
  const before = start > 0 ? text.slice(0, start).at(-1) ?? "" : "";
  const after = end < text.length ? text.slice(end).at(0) ?? "" : "";
  return !WORD_CHARACTER.test(before) && !WORD_CHARACTER.test(after);
}

/** Find every occurrence of `query`, in file order. */
export function searchLines(
  lines: readonly SourceLine[],
  query: string,
  options: SourceSearchOptions = {},
): SourceMatch[] {
  if (!Array.isArray(lines) || typeof query !== "string" || query.length === 0) return [];
  const limit = options.limit ?? DEFAULT_LIMIT;
  const caseSensitive = options.caseSensitive ?? false;
  const needle = caseSensitive ? query : foldWithMap(query).folded;
  if (needle.length === 0) return [];
  const matches: SourceMatch[] = [];

  for (const line of lines) {
    const { folded, map } = caseSensitive
      ? { folded: line.text, map: null as number[] | null }
      : foldWithMap(line.text);
    let from = 0;
    for (;;) {
      if (matches.length >= limit) return matches;
      const found = folded.indexOf(needle, from);
      if (found === -1) break;
      const start = map ? map[found] : found;
      const end = map ? map[found + needle.length] ?? line.text.length : found + needle.length;
      if (end > start && (!options.wholeWord || isWordBoundary(line.text, start, end))) {
        // Collapse folding expansions that map to the same source range.
        const last = matches[matches.length - 1];
        if (!last || last.line !== line.number || last.start !== start || last.end !== end) {
          matches.push({ line: line.number, start, end });
        }
      }
      // Advance one code point so overlapping occurrences stay reachable
      // without ever landing inside a surrogate pair.
      from = nextBoundary(folded, found);
    }
  }
  return matches;
}

/**
 * Index of the first match at or after `line`, wrapping to the start.
 * Returns -1 when there are no matches at all.
 */
export function matchIndexAtOrAfter(matches: readonly SourceMatch[], line: number): number {
  if (matches.length === 0) return -1;
  const found = matches.findIndex((match) => match.line >= line);
  return found === -1 ? 0 : found;
}

/** Step through matches with wrap-around. */
export function stepMatch(total: number, current: number, delta: number): number {
  if (total <= 0) return -1;
  if (current < 0) return delta >= 0 ? 0 : total - 1;
  return (((current + delta) % total) + total) % total;
}

/** One rendered run of a line: plain, matched, or the active match. */
export interface LineSegment {
  text: string;
  matched: boolean;
  active: boolean;
}

/**
 * Split a line into alternating plain and matched runs.
 *
 * `active` marks the match the reader is currently on, so the viewer can
 * distinguish it from the others without a second pass.
 */
export function segmentLine(
  text: string,
  matches: readonly SourceMatch[],
  active?: SourceMatch | null,
): LineSegment[] {
  if (matches.length === 0) return [{ text, matched: false, active: false }];
  const segments: LineSegment[] = [];
  let cursor = 0;
  for (const match of matches) {
    // Overlapping matches would double-render; keep the first one.
    if (match.start < cursor) continue;
    if (match.start > cursor) {
      segments.push({ text: text.slice(cursor, match.start), matched: false, active: false });
    }
    const isActive =
      !!active && active.line === match.line && active.start === match.start && active.end === match.end;
    segments.push({ text: text.slice(match.start, match.end), matched: true, active: isActive });
    cursor = match.end;
  }
  if (cursor < text.length) {
    segments.push({ text: text.slice(cursor), matched: false, active: false });
  }
  return segments;
}

/**
 * A highlight range that may span several lines, expressed per line.
 *
 * Used for a diff hunk or a tool range: the caller says "lines 12 through 18",
 * and each rendered line learns whether it is inside, and whether it is the
 * first or last line of the range so the viewer can round the ends.
 */
export interface RangeHighlight {
  firstLine: number;
  lastLine: number;
}

export type RangePosition = "outside" | "only" | "first" | "middle" | "last";

export function rangePosition(range: RangeHighlight | null, line: number): RangePosition {
  if (!range) return "outside";
  const first = Math.min(range.firstLine, range.lastLine);
  const last = Math.max(range.firstLine, range.lastLine);
  if (line < first || line > last) return "outside";
  if (first === last) return "only";
  if (line === first) return "first";
  if (line === last) return "last";
  return "middle";
}

/** Screen-reader announcement for the current search position. */
export function searchStatus(total: number, current: number, query: string): string {
  if (!query) return "";
  if (total === 0) return `No matches for ${query}`;
  const position = current >= 0 ? current + 1 : 1;
  return `Match ${position} of ${total} for ${query}`;
}
