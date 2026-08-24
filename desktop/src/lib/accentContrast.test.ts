import { describe, expect, it } from "vitest";
import { readFileSync } from "fs";
import { dirname, join } from "path";
import { fileURLToPath } from "url";

const root = dirname(fileURLToPath(import.meta.url));
const css = readFileSync(join(root, "..", "styles", "app.css"), "utf8");

const ACCENTS = ["amber", "teal", "violet"] as const;

function srgbToLin(channel: number): number {
  const c = channel / 255;
  return c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
}

function relativeLuminance(hex: string): number {
  const h = hex.replace("#", "");
  const r = parseInt(h.slice(0, 2), 16);
  const g = parseInt(h.slice(2, 4), 16);
  const b = parseInt(h.slice(4, 6), 16);
  return 0.2126 * srgbToLin(r) + 0.7152 * srgbToLin(g) + 0.0722 * srgbToLin(b);
}

function contrastRatio(a: string, b: string): number {
  const l1 = relativeLuminance(a);
  const l2 = relativeLuminance(b);
  const [hi, lo] = l1 > l2 ? [l1, l2] : [l2, l1];
  return (hi + 0.05) / (lo + 0.05);
}

function firstBlock(selector: string): string {
  const idx = css.indexOf(selector);
  expect(idx, `missing selector ${selector}`).toBeGreaterThan(-1);
  const start = css.indexOf("{", idx);
  let depth = 0;
  for (let i = start; i < css.length; i++) {
    if (css[i] === "{") depth += 1;
    else if (css[i] === "}") {
      depth -= 1;
      if (depth === 0) return css.slice(start + 1, i);
    }
  }
  throw new Error(`unclosed block for ${selector}`);
}

function tokenHex(block: string, token: string): string {
  const m = block.match(new RegExp(`${token}:\\s*(#[0-9a-fA-F]{6})`));
  expect(m, `missing ${token} hex in block`).toBeTruthy();
  return m![1];
}

describe("accent contrast contract", () => {
  it("does not let unscoped html[data-accent] clobber light tokens", () => {
    expect(css).not.toMatch(/html\[data-accent="amber"\]\s*\{/);
    expect(css).not.toMatch(/html\[data-accent="teal"\]\s*\{/);
    expect(css).not.toMatch(/html\[data-accent="violet"\]\s*\{/);
    for (const accent of ACCENTS) {
      expect(css).toContain(`html:not([data-theme="light"])[data-accent="${accent}"]`);
      expect(css).toContain(`html[data-theme="light"][data-accent="${accent}"]`);
    }
  });

  it("keeps default and accent --accent ≥ 4.5:1 on the light canvas", () => {
    const light = firstBlock('[data-theme="light"]');
    const bg = tokenHex(light, "--bg");
    const defaultAccent = tokenHex(light, "--accent");
    const ok = tokenHex(light, "--ok");
    expect(contrastRatio(defaultAccent, bg)).toBeGreaterThanOrEqual(4.5);
    expect(contrastRatio(ok, bg)).toBeGreaterThanOrEqual(4.5);
    for (const accent of ACCENTS) {
      const block = firstBlock(
        `html[data-theme="light"][data-accent="${accent}"]`,
      );
      const color = tokenHex(block, "--accent");
      expect(
        contrastRatio(color, bg),
        `${accent} accent ${color} on ${bg}`,
      ).toBeGreaterThanOrEqual(4.5);
    }
  });

  it("keeps dark accent overrides ≥ 4.5:1 on the dark canvas", () => {
    const dark = firstBlock('[data-theme="dark"]');
    const bg = tokenHex(dark, "--bg");
    for (const accent of ACCENTS) {
      const block = firstBlock(
        `html:not([data-theme="light"])[data-accent="${accent}"]`,
      );
      const color = tokenHex(block, "--accent");
      expect(contrastRatio(color, bg)).toBeGreaterThanOrEqual(4.5);
    }
  });
});
