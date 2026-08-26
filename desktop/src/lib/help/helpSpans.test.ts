import { describe, expect, it } from "vitest";
import { HELP_CORPUS, getHelpChunk } from "./canonical/corpus";
import {
  HELP_MAX_QUOTE_CODE_POINTS,
  buildHelpClaimSpan,
  mapSanitizedRangeToSource,
  sanitizeWithOffsetMap,
  verifyHelpClaimSpan,
  type HelpClaimSpan,
} from "./retrieval/spans";

const CHUNK_ID = "operations.durable-recovery#en.body.0";

describe("corpus normalization", () => {
  it("stores every chunk in NFC so spans live in one coordinate system", () => {
    // Mixed forms would make the same visible character occupy a different
    // number of code points depending on the article it came from.
    for (const chunk of HELP_CORPUS.chunks) {
      expect(chunk.text.normalize("NFC"), chunk.id).toBe(chunk.text);
    }
    for (const article of HELP_CORPUS.articles) {
      expect(article.title.normalize("NFC"), article.id).toBe(article.title);
    }
  });
});

describe("claim spans", () => {
  it("locates a quote and verifies against the corpus", () => {
    const chunk = getHelpChunk(CHUNK_ID);
    expect(chunk).toBeDefined();
    const quote = chunk!.text.slice(0, 24);
    const span = buildHelpClaimSpan(CHUNK_ID, quote);
    expect(span).not.toBeNull();
    expect(span!.quote).toBe(quote);
    expect(verifyHelpClaimSpan(span!)).toEqual({ ok: true });
  });

  it("refuses to cite a quote that is not in the chunk", () => {
    expect(buildHelpClaimSpan(CHUNK_ID, "this sentence does not appear")).toBeNull();
    expect(buildHelpClaimSpan("no.such#chunk", "anything")).toBeNull();
    expect(buildHelpClaimSpan(CHUNK_ID, "")).toBeNull();
  });

  it("matches a decomposed quote against the composed corpus", () => {
    const chunk = getHelpChunk("operations.durable-recovery#es.title.0");
    expect(chunk).toBeDefined();
    // "Recuperar una ejecución duradera" contains a precomposed ó; a caller
    // supplying NFD must still land on the right characters.
    // Slice far enough to include the precomposed "ó".
    const composed = chunk!.text.slice(0, 25);
    const decomposed = composed.normalize("NFD");
    expect(decomposed).not.toBe(composed);
    const span = buildHelpClaimSpan(chunk!.id, decomposed);
    expect(span).not.toBeNull();
    expect(span!.quote).toBe(composed);
    expect(verifyHelpClaimSpan(span!)).toEqual({ ok: true });
  });

  it.each([
    ["out-of-range", (span: HelpClaimSpan) => ({ ...span, endUtf16: 1_000_000 })],
    ["out-of-range", (span: HelpClaimSpan) => ({ ...span, startUtf16: span.endUtf16 })],
    ["quote-mismatch", (span: HelpClaimSpan) => ({ ...span, startUtf16: span.startUtf16 + 1 })],
    ["code-point-mismatch", (span: HelpClaimSpan) => ({ ...span, startCodePoint: span.startCodePoint + 3 })],
    ["unknown-chunk", (span: HelpClaimSpan) => ({ ...span, chunkId: "no.such#chunk" })],
    ["empty-quote", (span: HelpClaimSpan) => ({ ...span, quote: "" })],
  ])("fails closed on a drifted span (%s)", (reason, mutate) => {
    const chunk = getHelpChunk(CHUNK_ID)!;
    const span = buildHelpClaimSpan(CHUNK_ID, chunk.text.slice(4, 30))!;
    const verification = verifyHelpClaimSpan(mutate(span));
    expect(verification.ok).toBe(false);
    if (!verification.ok) expect(verification.reason).toBe(reason);
  });

  it("reports code-point offsets that differ from UTF-16 offsets for astral text", () => {
    // Not corpus text: the point is that the arithmetic is correct for text
    // the corpus could legitimately gain later.
    const text = "🚀🚀 restart duplicate";
    const utf16Index = text.indexOf("restart");
    expect(utf16Index).toBe(5);
    let codePoints = 0;
    for (let index = 0; index < utf16Index; ) {
      const point = text.codePointAt(index)!;
      index += point > 0xffff ? 2 : 1;
      codePoints += 1;
    }
    // Two emoji occupy four code units but three code points precede "restart".
    expect(codePoints).toBe(3);
    expect(codePoints).not.toBe(utf16Index);
  });

  it("rejects an over-long quote in code points, not code units", () => {
    const chunk = getHelpChunk(CHUNK_ID)!;
    const span = buildHelpClaimSpan(CHUNK_ID, chunk.text.slice(0, 20))!;
    const oversized = { ...span, quote: "a".repeat(HELP_MAX_QUOTE_CODE_POINTS + 1) };
    const verification = verifyHelpClaimSpan(oversized);
    expect(verification.ok).toBe(false);
    if (!verification.ok) expect(verification.reason).toBe("quote-too-long");
  });
});

describe("sanitization offset map", () => {
  it("keeps every mapped index inside the source", () => {
    const source = "  durable​ runs‮ expose \t a  state  ";
    const sanitized = sanitizeWithOffsetMap(source);
    expect(sanitized.start.length).toBe(sanitized.text.length);
    expect(sanitized.end.length).toBe(sanitized.text.length);
    for (const index of [...sanitized.start, ...sanitized.end]) {
      expect(index).toBeGreaterThanOrEqual(0);
      expect(index).toBeLessThanOrEqual(source.length);
    }
    expect(sanitized.text).not.toContain("​");
    expect(sanitized.text).not.toContain("‮");
  });

  it("maps a sanitized range back onto the characters it came from", () => {
    const source = "durable​ runs expose a state";
    const sanitized = sanitizeWithOffsetMap(source);
    const start = sanitized.text.indexOf("runs");
    const range = mapSanitizedRangeToSource(sanitized, start, start + 4);
    expect(range).not.toBeNull();
    // The zero-width character before "runs" shifted the source offset; the
    // map is what keeps the citation pointing at the right characters.
    expect(source.slice(range!.start, range!.end)).toBe("runs");
    expect(range!.start).toBeGreaterThan(start);
  });

  it("survives astral characters without splitting them", () => {
    const source = "restart 🚀 duplicate";
    const sanitized = sanitizeWithOffsetMap(source);
    const start = sanitized.text.indexOf("duplicate");
    const range = mapSanitizedRangeToSource(sanitized, start, start + "duplicate".length);
    expect(source.slice(range!.start, range!.end)).toBe("duplicate");
    // A split surrogate would render as a replacement character.
    expect(sanitized.text).toContain("🚀");
    expect([...sanitized.text].every((character) => character !== "�")).toBe(true);
  });

  it("keeps combining marks attached to their base character", () => {
    const source = "café recovery";
    const sanitized = sanitizeWithOffsetMap(source);
    expect(sanitized.text).toContain("café");
    const start = sanitized.text.indexOf("recovery");
    const range = mapSanitizedRangeToSource(sanitized, start, start + "recovery".length);
    expect(source.slice(range!.start, range!.end)).toBe("recovery");
  });

  it("rejects an out-of-bounds or inverted range", () => {
    const sanitized = sanitizeWithOffsetMap("durable runs");
    expect(mapSanitizedRangeToSource(sanitized, -1, 4)).toBeNull();
    expect(mapSanitizedRangeToSource(sanitized, 4, 4)).toBeNull();
    expect(mapSanitizedRangeToSource(sanitized, 0, 10_000)).toBeNull();
  });
});
