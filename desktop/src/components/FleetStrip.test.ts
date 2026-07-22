import { describe, expect, it } from "vitest";
import { readFileSync } from "fs";
import { dirname, join } from "path";
import { fileURLToPath } from "url";

const root = dirname(fileURLToPath(import.meta.url));

describe("FleetStrip cockpit exceed (#174 continuous)", () => {
  it("exposes subagent/token metrics and a11y labels on cards", () => {
    const src = readFileSync(join(root, "FleetStrip.tsx"), "utf8");
    expect(src).toMatch(/runningSubagents/);
    expect(src).toMatch(/totalTokens/);
    expect(src).toMatch(/aria-pressed/);
    expect(src).toMatch(/aria-label/);
    expect(src).toMatch(/data-testid="fleet-card"/);
    expect(src).toMatch(/data-running-subagents/);
  });
});
