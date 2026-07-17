import { readFileSync } from "fs";
import { dirname, join } from "path";
import { fileURLToPath } from "url";
import { describe, expect, it } from "vitest";

const root = dirname(fileURLToPath(import.meta.url));

describe("light theme tokens (#133)", () => {
  it("defines [data-theme=light] token overrides for core surfaces", () => {
    const css = readFileSync(join(root, "..", "styles", "app.css"), "utf8");
    expect(css).toMatch(/\[data-theme=["']light["']\]/);
    expect(css).toMatch(/\[data-theme=["']dark["']\]/);
    const lightBlock = css.slice(css.indexOf('[data-theme="light"]'));
    expect(lightBlock).toMatch(/--bg:\s*#/);
    expect(lightBlock).toMatch(/--bg-panel:\s*#/);
    expect(lightBlock).toMatch(/--text:\s*#/);
    expect(lightBlock).toMatch(/--border:\s*#/);
    expect(lightBlock).toMatch(/--accent:\s*#/);
    expect(lightBlock).toMatch(/--status-bar-bg:/);
    expect(lightBlock).toMatch(/--ctx-menu-bg:/);
    expect(lightBlock).toMatch(/--hover:/);
  });

  it("critical chrome uses theme tokens not raw dark hex", () => {
    const css = readFileSync(join(root, "..", "styles", "app.css"), "utf8");
    // Status bar / context menu / button hover must not hardcode dark hex.
    const status = css.slice(css.indexOf(".status-bar {"), css.indexOf(".status-bar {") + 400);
    expect(status).toMatch(/background:\s*var\(--status-bar-bg\)/);
    const ctx = css.slice(css.indexOf(".ctx-menu {"), css.indexOf(".ctx-menu {") + 500);
    expect(ctx).toMatch(/background:\s*var\(--ctx-menu-bg\)/);
    const hover = css.slice(css.indexOf("button:hover {"), css.indexOf("button:hover {") + 120);
    expect(hover).toMatch(/background:\s*var\(--hover\)/);
    expect(css).toMatch(/\.settings-header[\s\S]{0,200}background:\s*var\(--overlay-soft\)/);
  });

  it("App applies data-theme from agent status appearance", () => {
    const app = readFileSync(join(root, "..", "App.tsx"), "utf8");
    expect(app).toMatch(/document\.documentElement\.dataset\.theme/);
    expect(app).toMatch(/st\.appearance === ["']light["']/);
  });

  it("Settings still stamps data-theme on change", () => {
    const settings = readFileSync(
      join(root, "..", "components", "SettingsPanel.tsx"),
      "utf8",
    );
    expect(settings).toMatch(/document\.documentElement\.dataset\.theme/);
    expect(settings).not.toMatch(/full light tokens ship over time/);
  });
});

describe("resume / continue (#38)", () => {
  it("registers slash commands and openTab calls sessionLoad", () => {
    const proto = readFileSync(join(root, "protocol.ts"), "utf8");
    expect(proto).toMatch(/cmd:\s*["']\/resume["']/);
    expect(proto).toMatch(/cmd:\s*["']\/continue["']/);
    const app = readFileSync(join(root, "..", "App.tsx"), "utf8");
    expect(app).toMatch(/prompt === ["']\/continue["']/);
    expect(app).toMatch(/prompt === ["']\/resume["']/);
    expect(app).toMatch(/setSessionBrowserOpen\(true\)/);
    expect(app).toMatch(/id:\s*["']resume["']/);
    // openTab must promote backend active session on hydrate
    expect(app).toMatch(/api\.sessionLoad\(summary\.id\)/);
  });
});
