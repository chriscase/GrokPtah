/**
 * Excerpt construction and text sanitization for Help results.
 *
 * Highlights are returned as offset ranges over plain text, never as markup.
 * A consumer wraps the ranges in whatever element it wants, so there is no
 * HTML string for an injection to ride in on and no `dangerouslySetInnerHTML`
 * anywhere in the surface.
 */
import { normalizeText, rawTokens } from "./text";

export const HELP_EXCERPT_MAX_CHARS = 320;
export const HELP_MAX_HIGHLIGHTS = 24;

/** A highlighted span, as offsets into the sanitized excerpt text. */
export type HelpHighlight = {
  readonly start: number;
  readonly length: number;
};

export type HelpExcerpt = {
  readonly text: string;
  readonly highlights: readonly HelpHighlight[];
  readonly truncated: boolean;
};

/**
 * Characters that can make rendered text lie about its own content:
 * C0/C1 controls, zero-width and directional-override marks, the BOM, and the
 * soft hyphen. Stripped from everything a consumer will display.
 */
const UNSAFE_CHARACTERS =
  /[\u0000-\u0008\u000B-\u001F\u007F-\u009F\u00AD\u200B-\u200F\u202A-\u202E\u2060-\u2064\u2066-\u2069\uFEFF]/g;

/**
 * Make text safe to render as plain text.
 *
 * This is not HTML escaping — nothing here emits HTML. It removes characters
 * that would let corpus or provider text spoof its own rendering, and
 * collapses whitespace so an excerpt cannot smuggle layout.
 */
export function sanitizeHelpText(value: string, maxChars = HELP_EXCERPT_MAX_CHARS): string {
  const cleaned = value.replace(UNSAFE_CHARACTERS, "").replace(/\s+/g, " ").trim();
  return cleaned.length > maxChars ? cleaned.slice(0, maxChars) : cleaned;
}

function findWindowStart(haystack: string, needles: readonly string[], width: number): number {
  if (needles.length === 0 || haystack.length <= width) return 0;
  const normalized = normalizeText(haystack);
  let earliest = -1;
  for (const needle of needles) {
    const index = normalized.indexOf(needle);
    if (index >= 0 && (earliest < 0 || index < earliest)) earliest = index;
  }
  if (earliest < 0) return 0;
  // Center the window on the first match, then pull forward to a word boundary
  // so the excerpt does not start mid-token.
  const start = Math.max(0, Math.min(earliest - Math.floor(width / 3), haystack.length - width));
  if (start === 0) return 0;
  const boundary = haystack.indexOf(" ", start);
  return boundary >= 0 && boundary - start < 24 ? boundary + 1 : start;
}

/**
 * Build a bounded excerpt around the best match, with highlight ranges.
 *
 * Ranges are computed on the *sanitized* string that is returned, so offsets
 * are always valid for the text the consumer renders.
 */
export function buildHelpExcerpt(
  source: string,
  matchedTerms: readonly string[],
  maxChars = HELP_EXCERPT_MAX_CHARS,
): HelpExcerpt {
  const safe = sanitizeHelpText(source, Number.MAX_SAFE_INTEGER);
  const needles = [
    ...new Set(matchedTerms.map((term) => normalizeText(term)).filter((term) => term.length > 1)),
  ];
  const start = findWindowStart(safe, needles, maxChars);
  const truncated = safe.length > maxChars;
  let text = safe.slice(start, start + maxChars).trim();
  if (truncated && start + maxChars < safe.length) {
    // Trim a trailing partial word so the excerpt ends cleanly.
    const lastSpace = text.lastIndexOf(" ");
    if (lastSpace > maxChars * 0.6) text = text.slice(0, lastSpace);
  }

  const highlights: HelpHighlight[] = [];
  const normalizedText = normalizeText(text);
  // Only whole tokens are highlighted; a substring hit inside a longer word
  // would draw the eye to something the ranker did not actually match.
  let cursor = 0;
  for (const token of rawTokens(text)) {
    const index = normalizedText.indexOf(token, cursor);
    if (index < 0) continue;
    cursor = index + token.length;
    if (
      !needles.some(
        (needle) => token === needle || token.startsWith(needle) || needle.startsWith(token),
      )
    ) {
      continue;
    }
    if (highlights.length >= HELP_MAX_HIGHLIGHTS) break;
    const previous = highlights[highlights.length - 1];
    if (previous && previous.start + previous.length === index) {
      highlights[highlights.length - 1] = {
        start: previous.start,
        length: previous.length + token.length,
      };
    } else {
      highlights.push({ start: index, length: token.length });
    }
  }

  return { text, highlights: Object.freeze(highlights), truncated };
}
