import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";
import * as uiCore from "./uiCore";

/**
 * The source viewer splits deliberately: policy and privileged filesystem
 * access live in the Rust command layer, while parsing, highlighting, and
 * search are pure and shippable to a browser. These tests hold that line, so
 * a later edit cannot quietly pull native or credential authority into the
 * public core.
 */

/** Modules that must stay browser-safe. */
const BROWSER_SAFE = [
  "sourceView.ts",
  "sourceLocator.ts",
  "sourceDiff.ts",
  "sourceHighlight.ts",
  "sourceSearch.ts",
] as const;

/** Vitest runs from the desktop package root. */
function read(name: string): string {
  return readFileSync(resolve(process.cwd(), "src/lib", name), "utf8");
}

describe("source viewer browser boundary", () => {
  it.each(BROWSER_SAFE)("%s imports no native or credential authority", (name) => {
    const text = read(name);
    for (const forbidden of ["@tauri-apps", 'from "./api"', 'from "./trusted"', "invoke("]) {
      expect(text).not.toContain(forbidden);
    }
  });

  it.each(BROWSER_SAFE)("%s performs no filesystem or network access", (name) => {
    const text = read(name);
    for (const forbidden of ["node:fs", "require(", "fetch(", "XMLHttpRequest", "WebSocket"]) {
      expect(text).not.toContain(forbidden);
    }
  });

  it("keeps the privileged adapter out of the public barrel", () => {
    const barrel = read("uiCore.ts");
    expect(barrel).not.toContain("useSourceViewer");
    expect(barrel).not.toContain("./api");
  });

  it("publishes the read-only inspection surface from the public core", () => {
    for (const name of [
      "parseSourceDocument",
      "parseSourceRoots",
      "pickSourceRoot",
      "sourceViewErrorSummary",
      "truncationNotice",
      "rootIdentityLabel",
      "parseSourceLocator",
      "findSourceLocatorSpans",
      "stripDiffPathPrefix",
      "parseUnifiedDiff",
      "firstChangedLine",
      "highlightLine",
      "highlightLines",
      "searchLines",
      "segmentLine",
    ]) {
      expect(name in uiCore, `${name} must be exported from the public UI core`).toBe(true);
    }
  });

  it("mirrors the Rust refusal codes exactly", () => {
    // The frontend explains refusals by code; a code added on one side and
    // not the other would silently degrade to the generic message.
    const rust = readFileSync(
      resolve(process.cwd(), "../crates/common/xai-source-view/src/error.rs"),
      "utf8",
    );
    const codes = Array.from(rust.matchAll(/=> "([a-z_]+)",/g)).map((match) => match[1]);
    expect(codes.length).toBeGreaterThan(10);
    expect([...uiCore.SOURCE_VIEW_ERROR_CODES].sort()).toEqual([...codes].sort());
  });
});
