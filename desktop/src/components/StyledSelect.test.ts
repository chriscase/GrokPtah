import { readFileSync } from "fs";
import { dirname, join } from "path";
import { fileURLToPath } from "url";
import { describe, expect, it } from "vitest";

const root = dirname(fileURLToPath(import.meta.url));

describe("StyledSelect (#126)", () => {
  it("Settings and composer no longer use native <select>", () => {
    const settings = readFileSync(join(root, "SettingsPanel.tsx"), "utf8");
    const app = readFileSync(join(root, "..", "App.tsx"), "utf8");
    expect(settings).toMatch(/StyledSelect/);
    expect(settings).not.toMatch(/<select[\s>]/);
    // Composer model/effort pickers
    expect(app).toMatch(/className="composer-select"/);
    expect(app).not.toMatch(/composer-pill[\s\S]{0,200}<select/);
  });

  it("exports a listbox-based dropdown (not a native select element)", () => {
    const src = readFileSync(join(root, "StyledSelect.tsx"), "utf8");
    expect(src).toMatch(/role="listbox"/);
    expect(src).toMatch(/role="option"/);
    expect(src).not.toMatch(/<select[\s>]/);
  });
});
