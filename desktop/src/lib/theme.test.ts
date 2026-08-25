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

function mixOver(foreground: string, alpha: number, background: string): string {
  const [fr, fg, fb] = hexToRgb(foreground);
  const [br, bg, bb] = hexToRgb(background);
  const hex = (value: number) => Math.round(value).toString(16).padStart(2, "0");
  return `#${hex(fr * alpha + br * (1 - alpha))}${hex(fg * alpha + bg * (1 - alpha))}${hex(fb * alpha + bb * (1 - alpha))}`;
}

function helpStyles(css: string): string {
  const start = css.indexOf(".help-center {");
  const end = css.indexOf(".section-title {");
  expect(start).toBeGreaterThan(-1);
  expect(end).toBeGreaterThan(start);
  return css.slice(start, end);
}

function themeBlock(css: string, marker: string): string {
  const start = css.indexOf(marker);
  expect(start).toBeGreaterThan(-1);
  return css.slice(start, css.indexOf("}", start) + 1);
}

describe("Help overlay and small-label contract", () => {
  it("keeps light amber/accent 10px labels at WCAG AA against Help surfaces", () => {
    const css = readFileSync(join(root, "..", "styles", "app.css"), "utf8");
    const lightBlock = themeBlock(css, '[data-theme="light"]');
    const label = lightBlock.match(/--accent-label:\s*(#[0-9a-fA-F]{6})/)?.[1];
    const panel = lightBlock.match(/--bg-panel:\s*(#[0-9a-fA-F]{6})/)?.[1];
    const page = lightBlock.match(/--bg:\s*(#[0-9a-fA-F]{6})/)?.[1];
    expect(label).toBeTruthy();
    expect(panel).toBeTruthy();
    expect(page).toBeTruthy();
    expect(contrastRatio(label!, panel!)).toBeGreaterThanOrEqual(4.5);
    expect(contrastRatio(label!, page!)).toBeGreaterThanOrEqual(4.5);

    const darkBlock = themeBlock(css, ":root,");
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
    expect(css).toMatch(/\.help-assistant-answer strong \{\n  color:\s*var\(--accent-label\)/);
    expect(css).toMatch(/\.sidebar-help-opener\s*\{[\s\S]*?min-width:\s*44px/);
    expect(css).toMatch(/\.sidebar-help-opener\s*\{[\s\S]*?min-height:\s*44px/);
  });

  it("keeps light Help confirm primary and focus-visible at measured WCAG contrast", () => {
    const css = readFileSync(join(root, "..", "styles", "app.css"), "utf8");
    const help = helpStyles(css);
    expect(help).toMatch(
      /\.help-semantic-confirm button\.primary,\s*\n\.help-assistant-confirm button\.primary \{\n  color:\s*var\(--accent-label\)/,
    );
    expect(help).toMatch(/outline:\s*2px solid var\(--focus-ring\)/);
    for (const rule of help.matchAll(/:focus-visible[^{]*\{[^}]+\}/g)) {
      if (rule[0].includes("Highlight")) continue;
      expect(rule[0]).toMatch(/var\(--focus-ring\)/);
      expect(rule[0]).not.toMatch(/var\(--accent\)/);
    }

    const light = themeBlock(css, '[data-theme="light"]');
    const dark = themeBlock(css, ":root,");
    const lightLabel = light.match(/--accent-label:\s*(#[0-9a-fA-F]{6})/)?.[1];
    const lightAccent = light.match(/--accent:\s*(#[0-9a-fA-F]{6})/)?.[1];
    const lightPanel = light.match(/--bg-panel:\s*(#[0-9a-fA-F]{6})/)?.[1];
    const lightFocus = light.match(/--focus-ring:\s*(#[0-9a-fA-F]{6})/)?.[1];
    const lightAccentBg = light.match(/--accent-bg:\s*rgba\(\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)\s*,\s*([0-9.]+)\s*\)/);
    const darkLabel = dark.match(/--accent-label:\s*(#[0-9a-fA-F]{6})/)?.[1];
    const darkAccent = dark.match(/--accent:\s*(#[0-9a-fA-F]{6})/)?.[1];
    const darkPanel = dark.match(/--bg-panel:\s*(#[0-9a-fA-F]{6})/)?.[1];
    const darkFocus = dark.match(/--focus-ring:\s*(#[0-9a-fA-F]{6})/)?.[1];
    const darkAccentBg = dark.match(/--accent-bg:\s*rgba\(\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)\s*,\s*([0-9.]+)\s*\)/);
    expect(lightLabel && lightAccent && lightPanel && lightFocus && lightAccentBg).toBeTruthy();
    expect(darkLabel && darkAccent && darkPanel && darkFocus && darkAccentBg).toBeTruthy();

    const lightButtonBg = mixOver(lightAccent!, Number(lightAccentBg![4]), lightPanel!);
    const darkButtonBg = mixOver(darkAccent!, Number(darkAccentBg![4]), darkPanel!);
    expect(contrastRatio(lightLabel!, lightButtonBg)).toBeGreaterThanOrEqual(4.5);
    expect(contrastRatio(lightLabel!, lightPanel!)).toBeGreaterThanOrEqual(4.5);
    expect(contrastRatio(lightFocus!, lightPanel!)).toBeGreaterThanOrEqual(3);
    expect(contrastRatio(darkLabel!, darkButtonBg)).toBeGreaterThanOrEqual(4.5);
    expect(contrastRatio(darkLabel!, darkPanel!)).toBeGreaterThanOrEqual(4.5);
    expect(contrastRatio(darkFocus!, darkPanel!)).toBeGreaterThanOrEqual(3);
  });

  it("sizes Help chrome, labels, and details relative to the existing type-scale mode", () => {
    const css = readFileSync(join(root, "..", "styles", "app.css"), "utf8");
    const help = helpStyles(css);
    expect(css).toMatch(/html\[data-type-scale=["']lg["']\] body,\s*\nhtml\[data-type-scale=["']lg["']\] #root \{\n  font-size:\s*15px;/);
    expect(help).toMatch(/\.help-center \{[\s\S]*?font-size:\s*1em;/);
    expect(help).not.toMatch(/font-size:\s*\d+(\.\d+)?px/);
    expect(help).toMatch(/\.help-eyebrow \{[\s\S]*?font-size:\s*calc\(10\s*\/\s*13\.5\s*\*\s*1em\)/);
    expect(help).toMatch(/\.help-search label,\s*\n\.help-topic-label \{[\s\S]*?font-size:\s*calc\(11\s*\/\s*13\.5\s*\*\s*1em\)/);
    expect(help).toMatch(/\.help-confirm-details \{[\s\S]*?max-height:\s*11em;/);
    expect(help).toMatch(/\.help-confirm-details \{[\s\S]*?font-size:\s*calc\(11\s*\/\s*12\s*\*\s*1em\)/);
    expect(help).toMatch(/min-height:\s*44px/);
  });

  it("covers Help confirm details and forced-color focus, selection, and disabled states", () => {
    const css = readFileSync(join(root, "..", "styles", "app.css"), "utf8");
    const help = helpStyles(css);
    const forced = help.slice(help.indexOf("@media (forced-colors: active)"));
    expect(forced).toMatch(/\.help-confirm-details,/);
    expect(forced).toMatch(/\.help-confirm-details summary:focus-visible/);
    expect(forced).toMatch(/border-color:\s*CanvasText/);
    expect(forced).toMatch(/background:\s*Canvas/);
    expect(forced).toMatch(/color:\s*CanvasText/);
    expect(forced).toMatch(/\.help-list-item\.is-selected,[\s\S]*?outline:\s*2px solid Highlight/);
    expect(forced).toMatch(/outline:\s*2px solid Highlight/);
    expect(forced).toMatch(/\.help-semantic-search:disabled,[\s\S]*?color:\s*GrayText;[\s\S]*?border-color:\s*GrayText;[\s\S]*?opacity:\s*1;/);
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
