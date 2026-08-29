import { describe, expect, it } from "vitest";

import { HELP_FORBIDDEN_CHARACTERS } from "./verify";

/**
 * The forbidden-character class is the boundary that keeps control and bidi
 * characters out of rendered text.
 *
 * It was previously written as literal bytes, which made the file binary to
 * git — the one class deciding what text may reach a renderer could not be
 * read in a diff. It is now escapes, and this pins the set so the rewrite
 * cannot have changed it and a later edit cannot quietly widen it.
 */
const EXPECTED = new Set<number>();
// C0 controls, except tab (0x09) and newline (0x0A), which are ordinary text.
for (let code = 0x00; code <= 0x08; code += 1) EXPECTED.add(code);
for (let code = 0x0b; code <= 0x1f; code += 1) EXPECTED.add(code);
// DEL and the C1 range.
for (let code = 0x7f; code <= 0x9f; code += 1) EXPECTED.add(code);
// LTR/RTL marks and the Arabic letter mark.
for (const code of [0x200e, 0x200f, 0x061c]) EXPECTED.add(code);
// Bidi embeddings and overrides.
for (let code = 0x202a; code <= 0x202e; code += 1) EXPECTED.add(code);
// Bidi isolates.
for (let code = 0x2066; code <= 0x2069; code += 1) EXPECTED.add(code);

describe("the forbidden character set", () => {
  it("matches exactly the documented code points across the BMP", () => {
    const wrong: string[] = [];
    for (let code = 0; code <= 0xffff; code += 1) {
      // Lone surrogates are not characters and cannot be tested this way.
      if (code >= 0xd800 && code <= 0xdfff) continue;
      const matched = HELP_FORBIDDEN_CHARACTERS.test(String.fromCharCode(code));
      if (matched !== EXPECTED.has(code)) {
        wrong.push(`U+${code.toString(16).toUpperCase().padStart(4, "0")}`);
      }
    }
    expect(wrong).toEqual([]);
  });

  it("permits tab and newline, which are ordinary text", () => {
    expect(HELP_FORBIDDEN_CHARACTERS.test("\t")).toBe(false);
    expect(HELP_FORBIDDEN_CHARACTERS.test("\n")).toBe(false);
    expect(HELP_FORBIDDEN_CHARACTERS.test("ordinary prose")).toBe(false);
  });

  it("rejects the characters an attacker would reach for", () => {
    // A right-to-left override can reverse how a quote reads without changing
    // a byte a reviewer would notice.
    expect(HELP_FORBIDDEN_CHARACTERS.test("Approved.\u202E")).toBe(true);
    // An escape sequence can rewrite a terminal line.
    expect(HELP_FORBIDDEN_CHARACTERS.test("Denied.\u001B[2K")).toBe(true);
    // A zero-width isolate can hide structure inside a claim.
    expect(HELP_FORBIDDEN_CHARACTERS.test("safe\u2066unsafe\u2069")).toBe(true);
  });
});
