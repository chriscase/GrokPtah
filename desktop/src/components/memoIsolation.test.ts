import { readFileSync } from "fs";
import { dirname, join } from "path";
import { fileURLToPath } from "url";
import { describe, expect, it } from "vitest";

const root = dirname(fileURLToPath(import.meta.url));

/**
 * Structural proof for #122: hot path components must use React.memo so
 * unchanged tab references skip re-render during another pane's stream.
 */
describe("render isolation (#122)", () => {
  it("SessionPane, MarkdownBody, ToolCallCard, FleetStrip export memoized components", () => {
    const files = [
      "SessionPane.tsx",
      "MarkdownBody.tsx",
      "ToolCallCard.tsx",
      "FleetStrip.tsx",
    ];
    for (const f of files) {
      const src = readFileSync(join(root, f), "utf8");
      expect(src, f).toMatch(/memo\s*\(/);
      expect(src, f).toMatch(/export const \w+ = memo/);
    }
  });

  it("MarkdownBody hoists components map (not inline each render)", () => {
    const src = readFileSync(join(root, "MarkdownBody.tsx"), "utf8");
    expect(src).toMatch(/const MD_COMPONENTS/);
    expect(src).toMatch(/components=\{MD_COMPONENTS\}/);
    const body = src.slice(src.indexOf("function MarkdownBody"));
    expect(body).not.toMatch(/components=\{\{/);
  });

  it("App persists open tabs by id key only (no per-token disk)", () => {
    const app = readFileSync(join(root, "..", "App.tsx"), "utf8");
    expect(app).toMatch(/openTabIdsKey/);
    expect(app).toMatch(/tabs\.map\(\(t\) => t\.id\)\.join/);
  });
});

describe("jump to latest (#123)", () => {
  it("SessionPane includes jump-to-latest control", () => {
    const src = readFileSync(join(root, "SessionPane.tsx"), "utf8");
    expect(src).toMatch(/jump-to-latest/);
    expect(src).toMatch(/Jump to latest/);
    expect(src).toMatch(/showJump/);
  });
});
