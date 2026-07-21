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
