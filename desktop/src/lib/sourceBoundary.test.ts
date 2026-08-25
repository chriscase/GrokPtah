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
  "sourceViewTransport.ts",
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
      "parseSourceRootSnapshot",
      "parseSourceReadCursor",
      "selectSourceRoot",
      "appendSourceChunk",
      "sourceViewErrorSummary",
      "isAuthorizationRefusal",
      "projectionNotice",
      "rootIdentityLabel",
      "parseSourceLocator",
      "findSourceLocatorSpans",
      "stripDiffPathPrefix",
      "parseUnifiedDiff",
      "readDiffEvidence",
      "firstChangedLine",
      "highlightLine",
      "highlightLines",
      "searchLines",
      "segmentLine",
      "rangePosition",
      "validateSourceReadRequest",
      "sourceReadPayload",
      "assertTransportComplete",
      "SOURCE_VIEW_OPERATIONS",
      "SOURCE_VIEW_CONTRACT",
    ]) {
      expect(name in uiCore, `${name} must be exported from the public UI core`).toBe(true);
    }
  });

  it("publishes no credential, path, or native surface", () => {
    for (const name of Object.keys(uiCore)) {
      expect(name).not.toMatch(/apiKey|token(Secret|Key)|absolutePath|invoke/i);
    }
  });

  it("mirrors the Rust refusal codes exactly", () => {
    // The frontend explains refusals by code; a code added on one side and
    // not the other would silently degrade to the generic message.
    const rust = readFileSync(
      resolve(process.cwd(), "../crates/common/xai-source-view/src/error.rs"),
      "utf8",
    );
    // Read the published list, not the match arms: `io_kind_label` also
    // returns snake_case strings and would otherwise be counted as codes.
    const published = rust.slice(rust.indexOf("pub const CODES")).split("];")[0];
    const codes = Array.from(published.matchAll(/"([a-z_]+)"/g)).map((match) => match[1]);
    expect(codes.length).toBeGreaterThan(20);
    expect([...uiCore.SOURCE_VIEW_ERROR_CODES].sort()).toEqual([...codes].sort());
  });

  it("keeps the reassembly rule identical on both sides of the boundary", () => {
    // The Rust `LineAssembler` and `appendSourceChunk` implement one rule; a
    // divergence would show as text duplicated or dropped at a chunk seam.
    const rust = readFileSync(
      resolve(process.cwd(), "../crates/common/xai-source-view/src/read.rs"),
      "utf8",
    );
    expect(rust).toContain("continues_previous");
    expect(rust).toContain("tail.number == head.number");
    const typescript = read("sourceView.ts");
    expect(typescript).toContain("chunk.continuesPrevious");
    expect(typescript).toContain("tail.number === head.number");
  });

  it("pins the contract identifiers on both sides", () => {
    const rust = readFileSync(
      resolve(process.cwd(), "../crates/common/xai-source-view/src/lib.rs"),
      "utf8",
    );
    expect(rust).toContain(`"${uiCore.SOURCE_VIEW_CONTRACT}"`);
    const snapshot = readFileSync(
      resolve(process.cwd(), "../crates/common/xai-source-view/src/snapshot.rs"),
      "utf8",
    );
    expect(snapshot).toContain(`"${uiCore.SOURCE_VIEW_REPLAY_POLICY}"`);
    expect(snapshot).toContain(`"${uiCore.SOURCE_VIEW_TOKEN_VERSION}"`);
  });
});
