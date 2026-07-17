import { describe, expect, it } from "vitest";
import { readFileSync } from "fs";
import { dirname, join } from "path";
import { fileURLToPath } from "url";
import { SLASH_COMMANDS } from "./protocol";

const root = dirname(fileURLToPath(import.meta.url));

describe("slash commands (#148)", () => {
  it("registers fork, resume, rename, export, and cd", () => {
    const cmds = SLASH_COMMANDS.map((c) => c.cmd);
    for (const need of ["/fork", "/resume", "/rename", "/export", "/cd"]) {
      expect(cmds, need).toContain(need);
    }
  });

  it("App implements handlers for high-value slash commands", () => {
    const app = readFileSync(join(root, "..", "App.tsx"), "utf8");
    expect(app).toMatch(/prompt === ["']\/fork["']/);
    expect(app).toMatch(/\/rename /);
    expect(app).toMatch(/prompt === ["']\/export["']/);
    expect(app).toMatch(/\/cd /);
    expect(app).toMatch(/sessionFork|sessionRename|exportTranscript|setProjectCwd/);
  });
});

describe("launch splash (#150)", () => {
  it("ships Djed splash gated on ready", () => {
    const splash = readFileSync(
      join(root, "..", "components", "LaunchSplash.tsx"),
      "utf8",
    );
    expect(splash).toMatch(/prefers-reduced-motion/);
    expect(splash).toMatch(/ready/);
    expect(splash).toMatch(/grokptah-djed/);
    const app = readFileSync(join(root, "..", "App.tsx"), "utf8");
    expect(app).toMatch(/LaunchSplash/);
    expect(app).toMatch(/workspaceRestored && status !== null/);
  });
});

describe("appearance chrome (#134)", () => {
  it("exposes accent, density, type scale with preview", () => {
    const settings = readFileSync(
      join(root, "..", "components", "SettingsPanel.tsx"),
      "utf8",
    );
    expect(settings).toMatch(/Accent/);
    expect(settings).toMatch(/Density/);
    expect(settings).toMatch(/Type scale/);
    expect(settings).toMatch(/appearance-preview/);
  });
});
