import { describe, expect, it } from "vitest";
import { normalizeMarkdownTables } from "./MarkdownBody";

describe("normalizeMarkdownTables", () => {
  it("splits glued GFM table rows onto their own lines", () => {
    const glued =
      "| Dimension | Score | Summary | |-----------|:-----:|---------| | Modularity | B | Good seams | | Documentation | A- | Excellent docs |";
    const out = normalizeMarkdownTables(glued);
    const lines = out.split("\n").map((l) => l.trim());
    expect(lines[0]).toMatch(/^\| Dimension \| Score \| Summary \|$/);
    expect(lines[1]).toMatch(/^\|?-{3,}/);
    expect(lines.some((l) => l.includes("Modularity"))).toBe(true);
    expect(lines.some((l) => l.includes("Documentation"))).toBe(true);
    // Must not stay one giant line
    expect(out.split("\n").length).toBeGreaterThanOrEqual(4);
  });

  it("leaves fenced code alone", () => {
    const src = "```\n| not | a | table |\n```\n";
    expect(normalizeMarkdownTables(src)).toBe(src);
  });

  it("keeps already-valid multiline tables intact", () => {
    const good = `| A | B |\n|---|---|\n| 1 | 2 |\n`;
    expect(normalizeMarkdownTables(good)).toBe(good);
  });
});
