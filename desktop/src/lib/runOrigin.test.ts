import { describe, expect, it } from "vitest";
import type { DurableRun } from "./protocol";
import { activeRunOrigin } from "./runOrigin";

function run(overrides: Partial<DurableRun> = {}): DurableRun {
  return {
    runId: "run-1",
    sessionId: "session-1",
    workspace: "/tmp/project",
    requestId: "request-1",
    clientId: "mcp",
    state: "running",
    bounds: { maxPromptBytes: 1000, maxRounds: 4, maxDurationMs: 1000 },
    promptPreview: "demo",
    startSeq: 1,
    endSeq: null,
    createdAt: "2026-08-12T00:00:00Z",
    updatedAt: "2026-08-12T00:00:01Z",
    terminalResult: null,
    finalResponse: null,
    errorCode: null,
    aggregates: {
      changes: [],
      tests: [],
      permissionsRequested: 0,
      permissionsGranted: 0,
      permissionsDenied: 0,
      usage: { promptTokens: 0, completionTokens: 0, totalTokens: 0, requests: 0 },
      verification: null,
    },
    progress: null,
    execution: null,
    ...overrides,
  };
}

describe("activeRunOrigin", () => {
  it("recognizes active MCP and desktop runs", () => {
    expect(activeRunOrigin([run()])).toBe("mcp");
    expect(activeRunOrigin([run({ clientId: "desktop" })])).toBe("desktop");
    expect(activeRunOrigin([run({ clientId: "unknown" })])).toBe("other");
  });

  it("ignores completed runs and returns no origin when idle", () => {
    expect(activeRunOrigin([run({ state: "completed" })])).toBeNull();
    expect(activeRunOrigin([])).toBeNull();
  });
});
