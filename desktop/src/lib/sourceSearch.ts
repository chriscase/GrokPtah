/**
 * In-file search over an already-loaded document.
 *
 * Matching is plain substring, never a user-supplied regular expression:
 * the viewer must not be able to hang on a pathological pattern typed into
 * a find box. Pure and browser-safe.
 */

import type { SourceLine } from "./sourceView";

/** One match, as a half-open range inside one line. */
export interface SourceMatch {
  /** 1-based file line number. */
  line: number;
  /** Index into `SourceLine.text`. */
  start: number;
  end: number;
}

export interface SourceSearchOptions {
  caseSensitive?: boolean;
  wholeWord?: boolean;
  /** Stop after this many matches so a broad query stays responsive. */
  limit?: number;
}

const DEFAULT_LIMIT = 5000;
const WORD_CHARACTER = /[A-Za-z0-9_]/;

function isWordBoundary(text: string, start: number, end: number): boolean {
  const before = start > 0 ? text.charAt(start - 1) : "";
  const after = end < text.length ? text.charAt(end) : "";
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
  const needle = caseSensitive ? query : query.toLowerCase();
  const matches: SourceMatch[] = [];

  for (const line of lines) {
    const haystack = caseSensitive ? line.text : line.text.toLowerCase();
    let from = 0;
    for (;;) {
      if (matches.length >= limit) return matches;
      const start = haystack.indexOf(needle, from);
      if (start === -1) break;
      const end = start + needle.length;
      if (!options.wholeWord || isWordBoundary(line.text, start, end)) {
        matches.push({ line: line.number, start, end });
      }
      // Advance by one so overlapping occurrences are still reachable.
      from = start + 1;
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

/** Split a line into alternating plain / matched segments for rendering. */
export function segmentLine(
  text: string,
  matches: readonly SourceMatch[],
): Array<{ text: string; matched: boolean }> {
  if (matches.length === 0) return [{ text, matched: false }];
  const segments: Array<{ text: string; matched: boolean }> = [];
  let cursor = 0;
  for (const match of matches) {
    // Overlapping matches would double-render; keep the first one.
    if (match.start < cursor) continue;
    if (match.start > cursor) segments.push({ text: text.slice(cursor, match.start), matched: false });
    segments.push({ text: text.slice(match.start, match.end), matched: true });
    cursor = match.end;
  }
  if (cursor < text.length) segments.push({ text: text.slice(cursor), matched: false });
  return segments;
}

/** Screen-reader announcement for the current search position. */
export function searchStatus(total: number, current: number, query: string): string {
  if (!query) return "";
  if (total === 0) return `No matches for ${query}`;
  const position = current >= 0 ? current + 1 : 1;
  return `Match ${position} of ${total} for ${query}`;
}
