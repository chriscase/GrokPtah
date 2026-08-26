import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";
import * as hostBarrel from "./host";
import * as publicBarrel from "./public";
import * as trustedBarrel from "./trusted";
import * as uiCoreBarrel from "./uiCore";

// Vitest runs with the desktop package as its root, so read the staged
// manifest from disk rather than through a jsdom-relative module URL.
const manifest = JSON.parse(
  readFileSync(resolve(process.cwd(), "public-package.json"), "utf8"),
) as {
  exports: Record<string, Record<string, unknown>>;
  files: string[];
};

/** Symbols that can only exist where a bearer token can be used. */
const TRUSTED_ONLY = [
  "GrokPtahClient",
  "GrokPtahOperations",
  "GrokPtahRemoteError",
  "GrokPtahCapabilityError",
  "GrokPtahHost",
  "GrokPtahHostWorkspace",
  "GrokPtahHostRun",
  "GrokPtahScopeError",
  "GROKPTAH_HOST_CONTRACT",
  "GROKPTAH_MAX_ROUNDS",
  "GROKPTAH_RECOVERY_POLL_TOOL",
  "assertGrokPtahScope",
  "assertGrokPtahRunScope",
  "parseGrokPtahScope",
  "parseGrokPtahRunScope",
  "negotiateGrokPtahCapabilities",
  "requireGrokPtahCapabilities",
  "createGrokPtahRunMonitor",
  "applyGrokPtahRunNotification",
  "validateGrokPtahBounds",
];

/** Symbols that belong to the browser-safe surface only. */
const BROWSER_ONLY = [
  "GrokPtahBrokerClient",
  "GrokPtahBrokerError",
  "promptQueueReducer",
  "createPromptQueueEntry",
  "applyAssistantStreamChunk",
  "HELP_ARTICLES",
  "searchHelp",
  "createExternalWorkerMonitor",
];

describe("trusted-host seam barrel", () => {
  it("exposes the trusted powers a host consumer needs", () => {
    for (const name of TRUSTED_ONLY) {
      expect(hostBarrel, `host barrel is missing ${name}`).toHaveProperty(name);
    }
    expect(typeof hostBarrel.GrokPtahHost).toBe("function");
    expect(hostBarrel.GROKPTAH_HOST_CONTRACT).toBe("grokptah.host.v1");
  });

  it("re-uses the published capability lattice rather than a second copy", () => {
    expect(hostBarrel.CAPABILITY_CONTRACT).toBe(publicBarrel.CAPABILITY_CONTRACT);
    expect(hostBarrel.parseCapabilitySet).toBe(publicBarrel.parseCapabilitySet);
    expect(hostBarrel.capabilityActionState).toBe(publicBarrel.capabilityActionState);
    expect(hostBarrel.findCapability).toBe(publicBarrel.findCapability);
  });

  it("is a trusted surface, not a second copy of the browser package", () => {
    for (const name of BROWSER_ONLY) {
      expect(hostBarrel, `host barrel re-published ${name}`).not.toHaveProperty(name);
    }
  });
});

describe("browser-safe barrels", () => {
  it("expose no bearer-capable symbol", () => {
    for (const name of TRUSTED_ONLY) {
      expect(publicBarrel, `public barrel leaked ${name}`).not.toHaveProperty(name);
      expect(uiCoreBarrel, `ui-core barrel leaked ${name}`).not.toHaveProperty(name);
    }
  });

  it("keep their transport-neutral contract", () => {
    expect(typeof publicBarrel.GrokPtahBrokerClient).toBe("function");
    expect(publicBarrel.HELP_CONTRACT).toBe("grokptah.help.v1");
    expect(publicBarrel.EXTERNAL_WORKER_CONTRACT).toBe("grokptah.external-workers.v1");
    expect(typeof publicBarrel.parseCapabilitySet).toBe("function");
    expect(typeof uiCoreBarrel.promptQueueReducer).toBe("function");
    expect(uiCoreBarrel).not.toHaveProperty("GrokPtahBrokerClient");
  });
});

describe("in-repo trusted adapter barrel", () => {
  it("keeps its prior surface by re-exporting the published seam", () => {
    for (const name of TRUSTED_ONLY) {
      expect(trustedBarrel, `trusted barrel is missing ${name}`).toHaveProperty(name);
    }
    // `trusted` historically also carried the Help corpus; that stays true.
    expect(trustedBarrel.HELP_CONTRACT).toBe("grokptah.help.v1");
    expect(typeof trustedBarrel.searchHelp).toBe("function");
    expect(trustedBarrel.GrokPtahHost).toBe(hostBarrel.GrokPtahHost);
  });
});

describe("published package manifest", () => {
  it("leaves the browser-safe entries exactly as published", () => {
    expect(manifest.exports["."]).toEqual({
      types: "./types/public.d.ts",
      import: "./grokptah-public.js",
    });
    expect(manifest.exports["./ui-core"]).toEqual({
      types: "./types/uiCore.d.ts",
      import: "./ui-core.js",
    });
    expect(manifest.files).toContain("grokptah-public.js");
    expect(manifest.files).toContain("ui-core.js");
    expect(manifest.files).toContain("types");
  });

  it("publishes the trusted seam under a separate, fenced subpath", () => {
    const host = manifest.exports["./host"];
    expect(host).toBeDefined();
    expect(host.browser).toBeNull();
    expect(host.worker).toBeNull();
    expect(host.default).toBeNull();
    expect(host.types).toBe("./types/host.d.ts");
    expect(host.import).toBe("./grokptah-host.js");
    expect(manifest.files).toContain("grokptah-host.js");
  });

  it("orders the browser fences ahead of the import target", () => {
    // Export conditions match in declaration order, so a browser resolver must
    // reach `null` before it can reach the bearer-capable bundle.
    const conditions = Object.keys(manifest.exports["./host"]);
    expect(conditions.indexOf("browser")).toBeLessThan(conditions.indexOf("import"));
    expect(conditions.indexOf("worker")).toBeLessThan(conditions.indexOf("import"));
  });

  it("keeps the trusted bundle out of the browser-safe entry points", () => {
    for (const entry of [manifest.exports["."], manifest.exports["./ui-core"]]) {
      expect(JSON.stringify(entry)).not.toContain("grokptah-host");
      expect(JSON.stringify(entry)).not.toContain("host.d.ts");
    }
  });
});
