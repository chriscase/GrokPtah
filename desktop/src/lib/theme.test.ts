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

describe("Help accessibility appearance", () => {
  const css = readFileSync(join(root, "..", "styles", "app.css"), "utf8");

  it("keeps Help headings ordered while inheriting the application text scale", () => {
    const scale = (selector: string) =>
      Number(css.match(new RegExp(`\\.${selector} \\{[^}]*font-size:\\s*([\\d.]+)em;`))?.[1]);
    const sectionHeading = scale("help-header h2");
    const articleHeading = scale("help-article h3");
    expect(sectionHeading).toBeGreaterThan(articleHeading);
    expect(articleHeading).toBeGreaterThan(1);
  });

  it("distinguishes hovered, selected and keyboard-active Help options in forced colors", () => {
    const forced = css.slice(css.indexOf("@media (forced-colors: active)"));
    expect(forced).toMatch(/\.help-list-item:hover \{[^}]*outline:\s*1px dotted Highlight;/);
    expect(forced).toMatch(/\.help-list-item\.is-selected > strong \{[^}]*text-decoration:\s*underline;/);
    expect(forced).toMatch(/\.help-list-item\.is-active \{[^}]*outline:\s*2px solid Highlight;/);
  });
});
