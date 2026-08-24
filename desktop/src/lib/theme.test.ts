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
    expect(app).toMatch(/(?:st|effectiveStatus)\.appearance === ["']light["']/);
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
    // openTab must use the serialized Lane-scope promotion path on hydrate.
    expect(app).toMatch(/queueLaneScopePromotion\(\s*summary\.id/);
    expect(app).toMatch(/archived \? api\.sessionInspect\(id\) : api\.sessionLoad\(id\)/);
  });
});

function hexToRgb(hex: string): [number, number, number] {
  const value = hex.replace("#", "");
  return [
    parseInt(value.slice(0, 2), 16),
    parseInt(value.slice(2, 4), 16),
    parseInt(value.slice(4, 6), 16),
  ];
}

function relativeLuminance([r, g, b]: [number, number, number]): number {
  const linear = [r, g, b].map((channel) => {
    const srgb = channel / 255;
    return srgb <= 0.04045 ? srgb / 12.92 : ((srgb + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * linear[0] + 0.7152 * linear[1] + 0.0722 * linear[2];
}

function contrastRatio(foreground: string, background: string): number {
  const lighter = Math.max(relativeLuminance(hexToRgb(foreground)), relativeLuminance(hexToRgb(background)));
  const darker = Math.min(relativeLuminance(hexToRgb(foreground)), relativeLuminance(hexToRgb(background)));
  return (lighter + 0.05) / (darker + 0.05);
}

describe("Help overlay and small-label contract", () => {
  it("keeps light amber/accent 10px labels at WCAG AA against Help surfaces", () => {
    const css = readFileSync(join(root, "..", "styles", "app.css"), "utf8");
    const lightStart = css.indexOf('[data-theme="light"]');
    const lightBlock = css.slice(lightStart, css.indexOf("}", lightStart) + 1);
    const label = lightBlock.match(/--accent-label:\s*(#[0-9a-fA-F]{6})/)?.[1];
    const panel = lightBlock.match(/--bg-panel:\s*(#[0-9a-fA-F]{6})/)?.[1];
    const page = lightBlock.match(/--bg:\s*(#[0-9a-fA-F]{6})/)?.[1];
    expect(label).toBeTruthy();
    expect(panel).toBeTruthy();
    expect(page).toBeTruthy();
    expect(contrastRatio(label!, panel!)).toBeGreaterThanOrEqual(4.5);
    expect(contrastRatio(label!, page!)).toBeGreaterThanOrEqual(4.5);

    const darkStart = css.indexOf(":root,");
    const darkBlock = css.slice(darkStart, css.indexOf("}", darkStart) + 1);
    const darkLabel = darkBlock.match(/--accent-label:\s*(#[0-9a-fA-F]{6})/)?.[1];
    const darkPanel = darkBlock.match(/--bg-panel:\s*(#[0-9a-fA-F]{6})/)?.[1];
    expect(darkLabel).toBeTruthy();
    expect(darkPanel).toBeTruthy();
    expect(contrastRatio(darkLabel!, darkPanel!)).toBeGreaterThanOrEqual(4.5);

    const amberStart = css.indexOf('html[data-accent="amber"]');
    const amberBlock = css.slice(amberStart, css.indexOf("}", amberStart) + 1);
    expect(amberBlock).not.toMatch(/--accent-label:/);

    expect(css).toMatch(/\.help-eyebrow[\s\S]{0,180}color:\s*var\(--accent-label\)/);
    expect(css).toMatch(/\.help-list-topic,\s*\n\.help-article-topic \{\n  color:\s*var\(--accent-label\)/);
  });

  it("defines a global overlay contract where consent stacks above Help", () => {
    const css = readFileSync(join(root, "..", "styles", "app.css"), "utf8");
    const help = Number(css.match(/--z-layer-help:\s*(\d+)/)?.[1]);
    const consent = Number(css.match(/--z-layer-consent:\s*(\d+)/)?.[1]);
    expect(help).toBeGreaterThan(0);
    expect(consent).toBeGreaterThan(help);
    expect(css).toMatch(/\.help-center\s*\{[\s\S]*?z-index:\s*var\(--z-layer-help\)/);
    expect(css).toMatch(/\[data-modal-layer=["']consent["']\][\s\S]{0,80}z-index:\s*var\(--z-layer-consent\)/);
  });
});
