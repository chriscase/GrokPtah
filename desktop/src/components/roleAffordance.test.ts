import { readFileSync } from "fs";
import { dirname, join } from "path";
import { fileURLToPath } from "url";
import { describe, expect, it } from "vitest";

const root = dirname(fileURLToPath(import.meta.url));

describe("role affordance (#127)", () => {
  it("SessionPane labels user and assistant bubbles", () => {
    const src = readFileSync(join(root, "SessionPane.tsx"), "utf8");
    expect(src).toMatch(/bubble-role/);
    expect(src).toMatch(/You/);
    expect(src).toMatch(/Grok/);
  });

  it("CSS differentiates user vs assistant roles", () => {
    const css = readFileSync(join(root, "..", "styles", "app.css"), "utf8");
    expect(css).toMatch(/\.bubble-role/);
    expect(css).toMatch(/\.bubble\.user \.bubble-role/);
    expect(css).toMatch(/\.bubble\.assistant \.bubble-role/);
    expect(css).toMatch(/border-left:\s*3px solid var\(--accent\)/);
  });
});
