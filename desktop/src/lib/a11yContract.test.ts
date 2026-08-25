import { readFileSync } from "fs";
import { dirname, join } from "path";
import { fileURLToPath } from "url";
import { describe, expect, it } from "vitest";

/**
 * Computed accessibility contracts for the operator stylesheet.
 *
 * These are ratchets, not one-time cleanups: each one is the check that would
 * have caught the finding it closes at authoring time. They need no browser.
 */
const here = dirname(fileURLToPath(import.meta.url));
const css = readFileSync(join(here, "..", "styles", "app.css"), "utf8");
const permissionModal = readFileSync(
  join(here, "..", "components", "PermissionModal.tsx"),
  "utf8",
);

const ACCENTS = ["amber", "teal", "violet"] as const;
/** Every ground a focus ring or accent label is actually painted on. */
const SURFACE_TOKENS = [
  "--bg",
  "--bg-panel",
  "--bg-elevated",
  "--bg-input",
] as const;

function srgbToLinear(channel: number): number {
  const c = channel / 255;
  return c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
}

function relativeLuminance(hex: string): number {
  const h = hex.replace("#", "");
  return (
    0.2126 * srgbToLinear(parseInt(h.slice(0, 2), 16)) +
    0.7152 * srgbToLinear(parseInt(h.slice(2, 4), 16)) +
    0.0722 * srgbToLinear(parseInt(h.slice(4, 6), 16))
  );
}

function contrastRatio(a: string, b: string): number {
  const [hi, lo] = [relativeLuminance(a), relativeLuminance(b)].sort(
    (x, y) => y - x,
  );
  return (hi + 0.05) / (lo + 0.05);
}

function blockAfter(selector: string): string {
  const index = css.indexOf(selector);
  expect(index, `missing selector ${selector}`).toBeGreaterThan(-1);
  const start = css.indexOf("{", index);
  let depth = 0;
  for (let i = start; i < css.length; i += 1) {
    if (css[i] === "{") depth += 1;
    else if (css[i] === "}") {
      depth -= 1;
      if (depth === 0) return css.slice(start + 1, i);
    }
  }
  throw new Error(`unclosed block for ${selector}`);
}

function tokenHex(block: string, token: string): string {
  const match = block.match(new RegExp(`${token}:\\s*(#[0-9a-fA-F]{6})`));
  expect(match, `missing ${token} hex`).toBeTruthy();
  return match![1];
}

const darkBlock = blockAfter('[data-theme="dark"]');
const lightBlock = blockAfter('[data-theme="light"]');
const THEMES = [
  { name: "dark", block: darkBlock },
  { name: "light", block: lightBlock },
] as const;

describe("focus indicator and accent contrast (WCAG 1.4.11 / 1.4.3)", () => {
  it("clears the 3:1 non-text floor on every surface, theme and accent", () => {
    for (const theme of THEMES) {
      const defaultAccent = tokenHex(theme.block, "--accent");
      for (const surfaceToken of SURFACE_TOKENS) {
        const surface = tokenHex(theme.block, surfaceToken);
        expect(
          contrastRatio(defaultAccent, surface),
          `${theme.name} default accent ${defaultAccent} on ${surfaceToken}`,
        ).toBeGreaterThanOrEqual(3);
      }
    }

    for (const accent of ACCENTS) {
      const dark = tokenHex(
        blockAfter(`html:not([data-theme="light"])[data-accent="${accent}"]`),
        "--accent",
      );
      const light = tokenHex(
        blockAfter(`html[data-theme="light"][data-accent="${accent}"]`),
        "--accent",
      );
      for (const surfaceToken of SURFACE_TOKENS) {
        expect(
          contrastRatio(dark, tokenHex(darkBlock, surfaceToken)),
          `dark ${accent} on ${surfaceToken}`,
        ).toBeGreaterThanOrEqual(3);
        expect(
          contrastRatio(light, tokenHex(lightBlock, surfaceToken)),
          `light ${accent} on ${surfaceToken}`,
        ).toBeGreaterThanOrEqual(3);
      }
    }
  });

  it("keeps accent, status and body text at 4.5:1 on panel and canvas", () => {
    for (const theme of THEMES) {
      for (const textToken of ["--text", "--muted", "--accent-label", "--danger", "--ok"]) {
        const color = tokenHex(theme.block, textToken);
        for (const surfaceToken of ["--bg", "--bg-panel"] as const) {
          expect(
            contrastRatio(color, tokenHex(theme.block, surfaceToken)),
            `${theme.name} ${textToken} on ${surfaceToken}`,
          ).toBeGreaterThanOrEqual(4.5);
        }
      }
    }
  });

  it("never paints an accent palette outside its theme", () => {
    for (const accent of ACCENTS) {
      expect(css).not.toMatch(
        new RegExp(`(^|\\n)html\\[data-accent="${accent}"\\]\\s*\\{`),
      );
      expect(css).toContain(`html:not([data-theme="light"])[data-accent="${accent}"]`);
      expect(css).toContain(`html[data-theme="light"][data-accent="${accent}"]`);
    }
  });

  it("leaves no focus suppression without a keyboard-visible replacement", () => {
    const suppressions = [...css.matchAll(/([^{}]+)\{[^{}]*outline:\s*none[^{}]*\}/g)].map(
      (match) => match[1].trim(),
    );
    expect(suppressions.length).toBeGreaterThan(0);
    for (const selector of suppressions) {
      expect(
        selector,
        `"${selector}" strips the focus ring unconditionally`,
      ).toContain(":not(:focus-visible)");
    }
    // The one global indicator still exists and is accent-driven.
    expect(css).toMatch(
      /:where\(button, input, select, textarea, \[tabindex\]\):focus-visible \{\s*outline: 2px solid var\(--accent\);/,
    );
  });

  it("offers a high-contrast escape hatch", () => {
    expect(css).toContain("@media (prefers-contrast: more)");
  });
});

describe("type scale actually scales (WCAG 1.4.4)", () => {
  it("carries no absolute font size anywhere in the stylesheet", () => {
    const absolute = [...css.matchAll(/font(?:-size)?:\s*[0-9.]+px/g)].map((m) => m[0]);
    expect(absolute).toEqual([]);
  });

  it("expresses every size as a root-relative token at or above a 10px floor", () => {
    const tokens = [...css.matchAll(/--fs-([0-9-]+):\s*calc\(([0-9.]+) \/ 13\.5 \* 1rem\)/g)];
    expect(tokens.length).toBeGreaterThanOrEqual(10);
    for (const [, name, value] of tokens) {
      expect(Number(value), `--fs-${name} is below the 10px floor`).toBeGreaterThanOrEqual(10);
    }
    // Every declaration resolves through those tokens.
    const declarations = [...css.matchAll(/font(?:-size)?:\s*var\(--fs-/g)];
    expect(declarations.length).toBeGreaterThan(200);
  });

  it("compounds density and type scale into one root size", () => {
    expect(css).toMatch(
      /html \{\s*font-size: calc\(var\(--density-root, 13\.5px\) \* var\(--type-scale, 1\)\);\s*\}/,
    );
    // Neither control may write an absolute font-size any more.
    for (const mode of ["compact", "comfortable", "spacious"]) {
      expect(blockAfter(`html[data-density="${mode}"]`)).toMatch(/--density-root:\s*[0-9.]+px/);
    }
    for (const mode of ["sm", "md", "lg"]) {
      expect(blockAfter(`html[data-type-scale="${mode}"]`)).toMatch(/--type-scale:\s*[0-9.]+/);
    }
    expect(css).not.toMatch(/html\[data-type-scale="[a-z]+"\] body/);
  });

  it("moves the dominant text sizes measurably at large type scale", () => {
    const scaleOf = (mode: string) =>
      Number(blockAfter(`html[data-type-scale="${mode}"]`).match(/--type-scale:\s*([0-9.]+)/)![1]);
    const rootOf = (mode: string) =>
      Number(
        blockAfter(`html[data-density="${mode}"]`).match(/--density-root:\s*([0-9.]+)px/)![1],
      );

    const base = rootOf("comfortable") * scaleOf("md");
    const large = rootOf("comfortable") * scaleOf("lg");
    expect(large / base).toBeGreaterThanOrEqual(1.1);

    // The most common label size — 11px — must grow with it, not stay pinned.
    const renderedAtLarge = (11 / 13.5) * large;
    expect(renderedAtLarge).toBeGreaterThanOrEqual(12.5);
    // And the smallest surviving size stays legible at the largest setting.
    expect((10 / 13.5) * rootOf("spacious") * scaleOf("lg")).toBeGreaterThanOrEqual(12);
  });
});

describe("forced colors reaches the safety-critical surfaces", () => {
  const forcedBlocks = [...css.matchAll(/@media \(forced-colors: active\)/g)];

  it("covers more than the Help Center", () => {
    expect(forcedBlocks.length).toBeGreaterThanOrEqual(2);
    for (const surface of [
      ".permission-risk",
      ".computer-approval",
      ".run-inspector",
      ".fleet-strip",
      ".session-tab",
      ".modal-actions button",
      ".help-center",
    ]) {
      const covered = css
        .slice(css.indexOf("@media (forced-colors: active)"))
        .includes(surface);
      expect(covered, `${surface} has no forced-colors rule`).toBe(true);
    }
  });

  it("re-expresses the deny tier without relying on colour", () => {
    expect(css).toMatch(
      /\.permission-risk\[data-tier="deny"\] \{[\s\S]*?border-width: 3px;[\s\S]*?outline: 2px solid Highlight;/,
    );
    // A stylesheet rule cannot reach an inline style, so the consent modal must
    // not carry one — that is what made the two tiers identical.
    expect(permissionModal).not.toMatch(/style=\{\{/);
    expect(permissionModal).toContain('data-tier={tier}');
  });

  it("still binds the focus ring to the system highlight", () => {
    expect(css).toMatch(
      /@media \(forced-colors: active\) \{\s*:where\(button, input, select, textarea, \[tabindex\]\):focus-visible \{\s*outline: 2px solid Highlight;/,
    );
  });

  it("leaves reduced-motion coverage intact", () => {
    expect(
      [...css.matchAll(/@media \(prefers-reduced-motion: reduce\)/g)].length,
    ).toBeGreaterThanOrEqual(6);
  });
});
