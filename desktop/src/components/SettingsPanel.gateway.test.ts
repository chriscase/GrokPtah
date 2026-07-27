import { describe, expect, it } from "vitest";
import { readFileSync } from "fs";
import { dirname, join } from "path";
import { fileURLToPath } from "url";

const root = dirname(fileURLToPath(import.meta.url));

describe("settings gateway UI (#169)", () => {
  it("exposes gateway fields and save control", () => {
    const settings = readFileSync(join(root, "SettingsPanel.tsx"), "utf8");
    expect(settings).toMatch(/settings-gateway/);
    expect(settings).toMatch(/gateway-base-url/);
    expect(settings).toMatch(/setGatewayConfig/);
    expect(settings).toMatch(/gateway\.json/);
  });
});

describe("subagent isolation setting (#195)", () => {
  it("makes shared-folder mutation an explicit unsafe choice", () => {
    const settings = readFileSync(join(root, "SettingsPanel.tsx"), "utf8");
    expect(settings).toMatch(/setSubagentIsolation/);
    expect(settings).toMatch(/Separate worktree \/ copy \(recommended\)/);
    expect(settings).toMatch(/Shared project folder \(unsafe\)/);
    expect(settings).toMatch(/mutating children can edit the same files/);
  });
});
