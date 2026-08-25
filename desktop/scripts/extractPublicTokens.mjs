/**
 * Extract the shared operator-UI layer from the desktop stylesheet.
 *
 * The audit's continuity finding was that no visual layer crossed the
 * desktop/browser boundary at all: `app.css` is imported by the desktop entry
 * and nothing else, so a browser consumer re-implemented the focus ring, the
 * screen-reader utility, the contrast escapes and every state colour from
 * scratch. Rather than fork a second stylesheet — which drifts — the regions
 * marked `@public-tokens` in `app.css` are staged verbatim into the public
 * package. One source, two consumers.
 *
 * The extractor is deliberately dumb: it copies marked text and then refuses
 * anything that would carry authority, desktop layout or component structure
 * across the boundary.
 */

export const START_MARKER = "/* @public-tokens:start";
export const END_MARKER = "/* @public-tokens:end */";

/** Tokens a consumer is entitled to rely on. Extraction fails without them. */
export const REQUIRED_TOKENS = [
  "--bg",
  "--bg-panel",
  "--bg-elevated",
  "--border",
  "--text",
  "--muted",
  "--accent",
  "--accent-label",
  "--danger",
  "--ok",
  "--fs-10",
  "--fs-11",
  "--fs-12",
  "--fs-13",
  "--density-root",
  "--type-scale",
];

/** Rules a consumer is entitled to rely on. */
export const REQUIRED_RULES = [
  ':root,\n[data-theme="dark"]',
  '[data-theme="light"]',
  ":focus-visible",
  ".sr-only",
  "@media (prefers-contrast: more)",
  "@media (forced-colors: active)",
];

/**
 * Anything that would make the shared layer more than a token/a11y contract.
 * `desktop`-only selectors are the sharp edge here: exporting `.permission-*`
 * or `.computer-*` would imply a component contract this package does not ship.
 */
const FORBIDDEN_PATTERNS = [
  /\.permission-/,
  /\.computer-/,
  /\.help-/,
  /\.session-tab/,
  /\.composer-/,
  /\.modal/,
  /\.sidebar/,
  /\.rightbar/,
  /\.titlebar/,
  /\.status-bar/,
  /tauri/i,
  /Bearer/i,
  /apiKey/i,
  /GROKPTAH_HOME/,
  /\/Users\//,
];

export function extractPublicTokens(css) {
  const regions = [];
  let cursor = 0;
  for (;;) {
    const start = css.indexOf(START_MARKER, cursor);
    if (start === -1) break;
    const end = css.indexOf(END_MARKER, start);
    if (end === -1) {
      throw new Error("unterminated @public-tokens region in app.css");
    }
    // Skip the marker comment itself, including any explanatory prose in it.
    const bodyStart = css.indexOf("*/", start) + 2;
    regions.push(css.slice(bodyStart, end).trim());
    cursor = end + END_MARKER.length;
  }
  if (regions.length === 0) {
    throw new Error("app.css declares no @public-tokens region");
  }

  const body = regions.join("\n\n");
  for (const pattern of FORBIDDEN_PATTERNS) {
    const match = body.match(pattern);
    if (match) {
      throw new Error(
        `@public-tokens region exports a non-token rule: ${match[0]}`,
      );
    }
  }
  for (const token of REQUIRED_TOKENS) {
    if (!body.includes(`${token}:`)) {
      throw new Error(`@public-tokens region is missing ${token}`);
    }
  }
  for (const rule of REQUIRED_RULES) {
    if (!body.includes(rule)) {
      throw new Error(`@public-tokens region is missing ${rule}`);
    }
  }

  return `/*
 * @grokptah/client — shared operator-UI tokens.
 *
 * Staged verbatim from desktop/src/styles/app.css. This is the whole shared
 * visual layer: colour tokens for both themes and all three accents, the type
 * scale and its density / type-scale controls, the focus indicator, the
 * high-contrast and forced-colors escapes, and the screen-reader utility.
 *
 * It is NOT a component library. Layout, component structure, focus management
 * and approval presentation remain the consumer's own.
 */
${body}
`;
}

export default extractPublicTokens;
