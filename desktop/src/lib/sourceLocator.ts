/**
 * Recognise file locations inside free text.
 *
 * Tool output, test failures, and diff headers all name files with a line
 * number in slightly different shapes. This module turns those shapes into a
 * single locator so a click opens the file at the right line instead of
 * asking a model to guess what was meant.
 *
 * It is pure and browser-safe: it validates nothing about the filesystem and
 * grants no access. Containment is decided by the Rust boundary, which sees
 * the locator only as a requested path.
 */

/** A file reference, with an optional 1-based position inside it. */
export interface SourceLocator {
  path: string;
  line: number | null;
  column: number | null;
}

const MAX_POSITION = 1_000_000_000;
/** Wrapping characters a path is commonly quoted or bracketed with. */
const WRAPPERS: ReadonlyArray<readonly [string, string]> = [
  ["'", "'"],
  ['"', '"'],
  ["`", "`"],
  ["(", ")"],
  ["[", "]"],
  ["<", ">"],
  ["\u201c", "\u201d"],
  ["\u2018", "\u2019"],
];

function position(raw: string): number | null {
  if (!/^\d{1,10}$/.test(raw)) return null;
  const value = Number.parseInt(raw, 10);
  if (!Number.isSafeInteger(value) || value < 1 || value > MAX_POSITION) return null;
  return value;
}

function unwrap(input: string): string {
  let value = input;
  let changed = true;
  while (changed) {
    changed = false;
    for (const [open, close] of WRAPPERS) {
      if (value.length > 1 && value.startsWith(open) && value.endsWith(close)) {
        value = value.slice(open.length, value.length - close.length).trim();
        changed = true;
      }
    }
  }
  return value;
}

/** True when the remainder is a Windows drive root such as `C:` or `C:\`. */
function isDriveRoot(value: string): boolean {
  return /^[A-Za-z]:[\\/]?$/.test(value);
}

/**
 * Strip the `a/` or `b/` prefix Git puts on unified-diff paths.
 *
 * Applied only by the diff reader, never by the generic parser: `a/thing.ts`
 * is a perfectly ordinary path in most repositories.
 */
export function stripDiffPathPrefix(path: string): string {
  const match = /^([abciow])\/(.+)$/.exec(path);
  return match ? match[2] : path;
}

/**
 * Parse a single token into a locator, or return null when it does not name
 * a file.
 *
 * Recognised shapes, after quotes and brackets are peeled away:
 *
 * * `src/a.rs`, `src/a.rs:12`, `src/a.rs:12:3`
 * * `src/a.rs(12)`, `src/a.rs(12,3)` — compiler style
 * * `src/a.rs, line 12` and `src/a.rs line 12` — runtime style
 * * `C:\repo\a.rs:12` — the drive colon is never read as a line number
 */
export function parseSourceLocator(raw: string): SourceLocator | null {
  if (typeof raw !== "string") return null;
  let value = unwrap(raw.trim());
  if (!value || value.includes("\0") || value.includes("\n")) return null;
  // A bare drive root names a volume, not a file. Checked before any
  // punctuation is peeled, so `C:` cannot decay into the path `C`.
  if (isDriveRoot(value)) return null;

  let line: number | null = null;
  let column: number | null = null;

  // `path(12,3)` / `path(12)`
  const parenthesised = /^(.*?)\((\d{1,10})(?:\s*,\s*(\d{1,10}))?\)$/.exec(value);
  if (parenthesised) {
    value = parenthesised[1].trim();
    line = position(parenthesised[2]);
    column = parenthesised[3] ? position(parenthesised[3]) : null;
    return finish(value, line, column);
  }

  // `path, line 12` / `path line 12`
  const spelled = /^(.*?)[,]?\s+line\s+(\d{1,10})(?:[,]?\s+col(?:umn)?\s+(\d{1,10}))?$/i.exec(value);
  if (spelled) {
    value = spelled[1].trim();
    line = position(spelled[2]);
    column = spelled[3] ? position(spelled[3]) : null;
    return finish(value, line, column);
  }

  // Trailing separators that carry no meaning (`a.rs:12:`, `a.rs:12,`).
  value = value.replace(/[:,;.]+$/, (tail) => (/^\.\w+$/.test(tail) ? tail : ""));

  // `path:12:3` then `path:12`, peeled right to left.
  for (let peeled = 0; peeled < 2; peeled += 1) {
    const match = /^(.*):(\d{1,10})$/.exec(value);
    if (!match) break;
    const head = match[1];
    if (!head || isDriveRoot(head)) break;
    const found = position(match[2]);
    if (found === null) break;
    if (line === null) {
      line = found;
    } else {
      column = line;
      line = found;
    }
    value = head;
  }

  return finish(value, line, column);
}

function finish(path: string, line: number | null, column: number | null): SourceLocator | null {
  const trimmed = path.trim();
  if (!trimmed || isDriveRoot(trimmed)) return null;
  // A bare number is a count, not a file.
  if (/^\d+$/.test(trimmed)) return null;
  return { path: trimmed, line, column: line === null ? null : column };
}

/**
 * Whether a locator's path looks like a file worth offering as a link.
 *
 * Deliberately conservative: it wants a directory separator or a short
 * extension, so prose like `see:12` is not turned into a broken link.
 */
export function looksLikeFilePath(path: string): boolean {
  if (!path || path.length > 4096) return false;
  if (/\s{2,}/.test(path)) return false;
  const name = path.split(/[\\/]/).pop() ?? "";
  if (!name) return false;
  const hasExtension = /\.[A-Za-z0-9][A-Za-z0-9+_-]{0,11}$/.test(name);
  const hasSeparator = /[\\/]/.test(path);
  const wellKnown = /^(Dockerfile|Makefile|Cargo\.lock|LICENSE|README)$/i.test(name);
  return hasExtension || wellKnown || (hasSeparator && name.length > 0);
}

/** Delimiters that never appear inside a path token we are willing to match. */
const TOKEN_SCAN = /[^\s"'`|]+/g;

/** A locator together with where it sits in the text it was found in. */
export interface SourceLocatorSpan {
  /** Index of the first character of the reference. */
  start: number;
  /** Index just past the last character. */
  end: number;
  /** The exact substring `[start, end)`, so rendering stays lossless. */
  text: string;
  locator: SourceLocator;
}

/** Shrink a span past wrapping quotes and brackets so the link is tight. */
function tighten(text: string, start: number, end: number): [number, number] {
  let from = start;
  let to = end;
  let changed = true;
  while (changed && to - from > 1) {
    changed = false;
    for (const [open, close] of WRAPPERS) {
      if (text.startsWith(open, from) && text.endsWith(close, to)) {
        from += open.length;
        to -= close.length;
        changed = true;
      }
    }
  }
  return [from, to];
}

/**
 * Locate every plausible file reference in a block of text, with the exact
 * character range each one occupies.
 *
 * Paths containing spaces are not recognised; that is the price of not
 * turning ordinary prose into links.
 */
export function findSourceLocatorSpans(text: string, limit = 200): SourceLocatorSpan[] {
  if (typeof text !== "string" || !text) return [];
  const spans: SourceLocatorSpan[] = [];
  for (const match of text.matchAll(TOKEN_SCAN)) {
    if (spans.length >= limit) break;
    const rawStart = match.index ?? 0;
    const [start, end] = tighten(text, rawStart, rawStart + match[0].length);
    const slice = text.slice(start, end);
    const locator = parseSourceLocator(slice);
    if (!locator || !looksLikeFilePath(locator.path)) continue;
    spans.push({ start, end, text: slice, locator });
  }
  return spans;
}

/**
 * Find every plausible file reference in a block of text, in order, without
 * duplicates.
 */
export function findSourceLocators(text: string, limit = 200): SourceLocator[] {
  const found: SourceLocator[] = [];
  const seen = new Set<string>();
  for (const span of findSourceLocatorSpans(text, Number.POSITIVE_INFINITY)) {
    if (found.length >= limit) break;
    const { locator } = span;
    const key = `${locator.path}:${locator.line ?? ""}:${locator.column ?? ""}`;
    if (seen.has(key)) continue;
    seen.add(key);
    found.push(locator);
  }
  return found;
}

/** Render a locator the way editors and stack traces spell it. */
export function formatSourceLocator(locator: SourceLocator): string {
  if (locator.line === null) return locator.path;
  if (locator.column === null) return `${locator.path}:${locator.line}`;
  return `${locator.path}:${locator.line}:${locator.column}`;
}
