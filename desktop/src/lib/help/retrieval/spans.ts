/**
 * Claim-level citation spans.
 *
 * A citation that names only an article is not checkable. A span names the
 * exact characters of the exact chunk a claim rests on, and carries enough
 * information to be *re-verified* against the corpus rather than trusted.
 *
 * Three things make spans survive real text:
 *
 * 1. **Two coordinate systems.** JavaScript string indices are UTF-16 code
 *    units, so an emoji is two units wide and a naive offset can split it.
 *    Spans carry UTF-16 offsets for JS slicing *and* code-point offsets for
 *    any consumer that does not use UTF-16.
 * 2. **The quote travels with the span.** `verifyHelpClaimSpan` re-derives the
 *    text from the corpus and compares. A span whose offsets drifted is
 *    detected instead of silently highlighting the wrong words.
 * 3. **Sanitization is mapped, not applied blindly.** Removing zero-width and
 *    bidi characters shifts every later offset. `sanitizeWithOffsetMap` keeps
 *    the correspondence, so a highlight computed on rendered text can still
 *    name its position in the canonical source.
 *
 * Corpus text is NFC by construction (enforced in `corpus.ts`), so spans live
 * in one normalization form and an incoming quote is normalized before it is
 * matched.
 */
import { getHelpChunk } from "../canonical/corpus";

/** Longest quote a claim may pin, in code points. */
export const HELP_MAX_QUOTE_CODE_POINTS = 320;

export type HelpClaimSpan = {
  readonly chunkId: string;
  /** Offsets in UTF-16 code units, for JS `slice`. */
  readonly startUtf16: number;
  readonly endUtf16: number;
  /** Offsets in Unicode code points, for consumers that are not UTF-16. */
  readonly startCodePoint: number;
  readonly endCodePoint: number;
  /** The exact text the span covers, as it appears in the chunk. */
  readonly quote: string;
};

export type HelpSpanVerification =
  | { readonly ok: true }
  | { readonly ok: false; readonly reason: HelpSpanFailure; readonly detail: string };

export type HelpSpanFailure =
  | "unknown-chunk"
  | "out-of-range"
  | "quote-mismatch"
  | "code-point-mismatch"
  | "splits-code-point"
  | "quote-too-long"
  | "empty-quote";

/** Count code points, not code units, up to a UTF-16 index. */
function codePointsBefore(text: string, utf16Index: number): number {
  let count = 0;
  for (let index = 0; index < utf16Index; ) {
    const point = text.codePointAt(index);
    if (point === undefined) break;
    index += point > 0xffff ? 2 : 1;
    count += 1;
  }
  return count;
}

/** True when the index falls inside a surrogate pair rather than between characters. */
function splitsCodePoint(text: string, index: number): boolean {
  if (index <= 0 || index >= text.length) return false;
  const previous = text.charCodeAt(index - 1);
  const current = text.charCodeAt(index);
  return previous >= 0xd800 && previous <= 0xdbff && current >= 0xdc00 && current <= 0xdfff;
}

/**
 * Build a span for a quote inside a chunk.
 *
 * The quote is matched in NFC, matching the corpus's own form, so a caller
 * that supplies decomposed text still lands on the right characters. Returns
 * null when the quote is not present — an unlocatable claim must not be
 * cited at all rather than cited approximately.
 */
export function buildHelpClaimSpan(chunkId: string, quote: string): HelpClaimSpan | null {
  const chunk = getHelpChunk(chunkId);
  if (!chunk) return null;
  const normalizedQuote = quote.normalize("NFC");
  if (normalizedQuote.length === 0) return null;
  if ([...normalizedQuote].length > HELP_MAX_QUOTE_CODE_POINTS) return null;

  const startUtf16 = chunk.text.indexOf(normalizedQuote);
  if (startUtf16 < 0) return null;
  const endUtf16 = startUtf16 + normalizedQuote.length;

  return Object.freeze({
    chunkId,
    startUtf16,
    endUtf16,
    startCodePoint: codePointsBefore(chunk.text, startUtf16),
    endCodePoint: codePointsBefore(chunk.text, endUtf16),
    quote: normalizedQuote,
  });
}

/**
 * Re-derive a span from the corpus and confirm it still says what it claims.
 *
 * Fails closed on every disagreement. This is what makes a citation checkable
 * by someone who did not produce it.
 */
export function verifyHelpClaimSpan(span: HelpClaimSpan): HelpSpanVerification {
  const chunk = getHelpChunk(span.chunkId);
  if (!chunk) return { ok: false, reason: "unknown-chunk", detail: span.chunkId };
  if (span.quote.length === 0) return { ok: false, reason: "empty-quote", detail: span.chunkId };
  if ([...span.quote].length > HELP_MAX_QUOTE_CODE_POINTS) {
    return { ok: false, reason: "quote-too-long", detail: String([...span.quote].length) };
  }
  if (
    span.startUtf16 < 0 ||
    span.endUtf16 > chunk.text.length ||
    span.startUtf16 >= span.endUtf16
  ) {
    return { ok: false, reason: "out-of-range", detail: `${span.startUtf16}..${span.endUtf16}` };
  }
  if (splitsCodePoint(chunk.text, span.startUtf16) || splitsCodePoint(chunk.text, span.endUtf16)) {
    return { ok: false, reason: "splits-code-point", detail: `${span.startUtf16}..${span.endUtf16}` };
  }
  const actual = chunk.text.slice(span.startUtf16, span.endUtf16);
  if (actual !== span.quote) {
    return { ok: false, reason: "quote-mismatch", detail: actual };
  }
  if (
    codePointsBefore(chunk.text, span.startUtf16) !== span.startCodePoint ||
    codePointsBefore(chunk.text, span.endUtf16) !== span.endCodePoint
  ) {
    return {
      ok: false,
      reason: "code-point-mismatch",
      detail: `${span.startCodePoint}..${span.endCodePoint}`,
    };
  }
  return { ok: true };
}

export type SanitizedWithMap = {
  readonly text: string;
  /** `start[i]` is where the source character that became `text[i]` begins. */
  readonly start: readonly number[];
  /**
   * `end[i]` is one past that source character.
   *
   * A separate end array is required, not a convenience: an exclusive end
   * offset cannot be read from the *next* entry's start, because collapsed
   * whitespace and removed characters leave a gap between them. Doing that
   * made a span over "runs" report "runs " — it swallowed the space that had
   * been collapsed away. For a surrogate pair both units share one start and
   * one end, so a range can never split a code point.
   */
  readonly end: readonly number[];
};

/** Characters removed before rendering. Must match `highlight.ts`. */
const UNSAFE_CHARACTERS =
  /[\u0000-\u0008\u000B-\u001F\u007F-\u009F\u00AD\u200B-\u200F\u202A-\u202E\u2060-\u2064\u2066-\u2069\uFEFF]/;

/**
 * Sanitize while keeping the correspondence back to the source.
 *
 * Whitespace runs collapse to a single space and unsafe characters are
 * dropped; both shift every later offset. Without the map, a highlight
 * computed on the rendered string cannot be turned back into a claim span,
 * and a citation would point at the wrong characters in the source.
 */
export function sanitizeWithOffsetMap(source: string): SanitizedWithMap {
  let text = "";
  const start: number[] = [];
  const end: number[] = [];
  let sourceIndex = 0;
  // Start of the whitespace run currently being collapsed, or -1.
  let pendingWhitespaceStart = -1;

  for (const character of source) {
    const width = character.length;
    if (UNSAFE_CHARACTERS.test(character)) {
      sourceIndex += width;
      continue;
    }
    if (/\s/.test(character)) {
      if (text.length > 0 && pendingWhitespaceStart < 0) pendingWhitespaceStart = sourceIndex;
      sourceIndex += width;
      continue;
    }
    if (pendingWhitespaceStart >= 0) {
      // The collapsed run spans from its first character to here.
      start.push(pendingWhitespaceStart);
      end.push(sourceIndex);
      text += " ";
      pendingWhitespaceStart = -1;
    }
    for (let unit = 0; unit < width; unit += 1) {
      start.push(sourceIndex);
      end.push(sourceIndex + width);
    }
    text += character;
    sourceIndex += width;
  }
  return { text, start: Object.freeze(start), end: Object.freeze(end) };
}

/**
 * Map a range in sanitized space back to source space.
 *
 * Returns null when the range is out of bounds, so a bad range cannot quietly
 * become a valid-looking span over the wrong text.
 */
export function mapSanitizedRangeToSource(
  sanitized: SanitizedWithMap,
  rangeStart: number,
  rangeEnd: number,
): { start: number; end: number } | null {
  if (rangeStart < 0 || rangeEnd > sanitized.text.length || rangeStart >= rangeEnd) return null;
  const sourceStart = sanitized.start[rangeStart];
  const sourceEnd = sanitized.end[rangeEnd - 1];
  if (sourceStart === undefined || sourceEnd === undefined || sourceStart >= sourceEnd) return null;
  return { start: sourceStart, end: sourceEnd };
}
