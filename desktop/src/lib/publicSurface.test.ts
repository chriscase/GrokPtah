import { readFileSync } from "fs";
import { dirname, join } from "path";
import { fileURLToPath } from "url";
import { describe, expect, it } from "vitest";
import * as uiCore from "./uiCore";
import * as publicApi from "./public";
import * as gatedHelp from "./help";
import { extractPublicTokens, REQUIRED_TOKENS } from "../../scripts/extractPublicTokens.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const readDesktop = (...parts: string[]) =>
  readFileSync(join(here, "..", ...parts), "utf8");
const css = readDesktop("styles", "app.css");
const manifest = JSON.parse(
  readFileSync(join(here, "..", "..", "public-package.json"), "utf8"),
);

/**
 * Names that belong to the capability-gated `grokptah.help.v1` corpus. Its
 * search defaults to public-only entries and it carries audience/access
 * metadata the live corpus has no equivalent for. It is a legitimate contract
 * for trusted adapters — it simply must not reach a browser consumer, where the
 * more inviting name used to resolve to it.
 */
const GATED_HELP_NAMES = [
  "HELP_CONTRACT",
  "HELP_ENTRIES",
  "buildHelpAssistantContext",
  "HELP_ASSISTANT_MAX_BYTES",
];

describe("one Help corpus reaches browser consumers", () => {
  it("binds every public search name to the same live corpus", () => {
    expect(uiCore.searchHelp).toBe(uiCore.searchHelpArticles);
    expect(publicApi.searchHelp).toBe(publicApi.searchHelpArticles);
    expect(publicApi.searchHelp).toBe(uiCore.searchHelp);
    expect(publicApi.HELP_ARTICLES).toBe(uiCore.HELP_ARTICLES);
    expect(uiCore.HELP_CORPUS_VERSION).toBe("product-corpus-v1");
  });

  it("ranks identically under both names", () => {
    const query = "restricted company gateway";
    const viaPlainName = uiCore.searchHelp(query);
    const viaExplicitName = uiCore.searchHelpArticles(query);
    expect(viaPlainName[0]?.article.id).toBe("providers.restricted-gateway-review");
    expect(viaPlainName.map((hit) => hit.article.id)).toEqual(
      viaExplicitName.map((hit) => hit.article.id),
    );
  });

  it("keeps the access-gated corpus off the browser surface", () => {
    for (const name of GATED_HELP_NAMES) {
      expect(name in uiCore, `uiCore still exports ${name}`).toBe(false);
      expect(name in publicApi, `public still exports ${name}`).toBe(false);
    }
    expect(readDesktop("lib", "uiCore.ts")).not.toMatch(/from "\.\/help"/);
  });

  it("leaves that corpus intact behind the trusted-adapter barrel", () => {
    // Not deleted — reachable only where a bearer token already is.
    expect(gatedHelp.HELP_CONTRACT).toBe("grokptah.help.v1");
    expect(readDesktop("lib", "trusted.ts")).toMatch(/export \* from "\.\/help"/);
    // And it is genuinely a different corpus, which is why the split mattered.
    expect(gatedHelp.HELP_ENTRIES.length).toBeGreaterThan(0);
    expect(
      gatedHelp.HELP_ENTRIES.every((entry) => "access" in entry && "audience" in entry),
    ).toBe(true);
    expect(uiCore.HELP_ARTICLES.some((article) => "access" in article)).toBe(false);
  });
});

describe("the shared visual layer is real and authority-free", () => {
  const tokens = extractPublicTokens(css);

  it("publishes the tokens a consumer needs to match the desktop", () => {
    for (const token of REQUIRED_TOKENS) {
      expect(tokens, `missing ${token}`).toContain(`${token}:`);
    }
    expect(tokens).toContain('[data-theme="light"]');
    expect(tokens).toContain('html[data-theme="light"][data-accent="violet"]');
    expect(tokens).toContain(".sr-only");
    expect(tokens).toContain(":focus-visible");
    expect(tokens).toContain("@media (prefers-contrast: more)");
    expect(tokens).toContain("@media (forced-colors: active)");
  });

  it("is a token contract, not a component library or an authority leak", () => {
    for (const componentish of [
      ".permission-",
      ".computer-",
      ".help-",
      ".composer-",
      ".modal",
      ".sidebar",
      ".titlebar",
    ]) {
      expect(tokens, `exported ${componentish}`).not.toContain(componentish);
    }
    for (const authority of ["tauri", "Bearer", "apiKey", "GROKPTAH_HOME", "/Users/"]) {
      expect(tokens.toLowerCase()).not.toContain(authority.toLowerCase());
    }
  });

  it("refuses to export a component or authority rule smuggled into a region", () => {
    const smuggled = css.replace(
      "/* @public-tokens:end */",
      ".permission-risk { color: red; }\n/* @public-tokens:end */",
    );
    expect(() => extractPublicTokens(smuggled)).toThrow(/non-token rule/);
    expect(() => extractPublicTokens("/* no markers here */")).toThrow(
      /declares no @public-tokens region/,
    );
  });

  it("ships through the package manifest under a resolvable subpath", () => {
    expect(manifest.exports["./styles/tokens.css"]).toBe("./styles/tokens.css");
    expect(manifest.files).toContain("styles");
  });

  it("stays derived from the desktop stylesheet, never forked beside it", () => {
    const stage = readFileSync(
      join(here, "..", "..", "scripts", "stage-public-package.mjs"),
      "utf8",
    );
    expect(stage).toContain("extractPublicTokens");
    expect(stage).toContain("src/styles/app.css");
    // No second stylesheet to drift from.
    expect(readDesktop("main.tsx")).toContain("app.css");
  });
});
