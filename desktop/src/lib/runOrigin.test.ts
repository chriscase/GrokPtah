import { describe, expect, it } from "vitest";
import type { DurableRun } from "./protocol";
import {
  activeRunOrigin,
  isLocalDesktopRunOrigin,
  isScopedDurableApprovalOrigin,
  LEGACY_DESKTOP_RUN_CLIENT_ID,
  LOCAL_DESKTOP_RUN_CLIENT_ID,
  MCP_RUN_CLIENT_ID,
  runRequiresDurableApproval,
} from "./runOrigin";

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

describe("durable approval origin", () => {
  it("reserves local-desktop as the sole host-minted local origin", () => {
    expect(isLocalDesktopRunOrigin(LOCAL_DESKTOP_RUN_CLIENT_ID)).toBe(true);
    expect(runRequiresDurableApproval(LOCAL_DESKTOP_RUN_CLIENT_ID)).toBe(false);
    expect(isScopedDurableApprovalOrigin(LOCAL_DESKTOP_RUN_CLIENT_ID)).toBe(false);
  });

  it("allows MCP and named external credentials to use scoped durable approval", () => {
    expect(isScopedDurableApprovalOrigin(MCP_RUN_CLIENT_ID)).toBe(true);
    expect(isScopedDurableApprovalOrigin("laptop")).toBe(true);
    expect(runRequiresDurableApproval(MCP_RUN_CLIENT_ID)).toBe(true);
    expect(runRequiresDurableApproval("laptop")).toBe(true);
    expect(isLocalDesktopRunOrigin("laptop")).toBe(false);
  });

  it("fails closed on legacy desktop and missing origins", () => {
    expect(isLocalDesktopRunOrigin(LEGACY_DESKTOP_RUN_CLIENT_ID)).toBe(false);
    expect(isScopedDurableApprovalOrigin(LEGACY_DESKTOP_RUN_CLIENT_ID)).toBe(false);
    expect(runRequiresDurableApproval(LEGACY_DESKTOP_RUN_CLIENT_ID)).toBe(true);
    expect(isLocalDesktopRunOrigin(null)).toBe(false);
    expect(isLocalDesktopRunOrigin(undefined)).toBe(false);
    expect(isScopedDurableApprovalOrigin(null)).toBe(false);
    expect(isScopedDurableApprovalOrigin(undefined)).toBe(false);
    expect(isScopedDurableApprovalOrigin("")).toBe(false);
    expect(runRequiresDurableApproval(null)).toBe(true);
    expect(runRequiresDurableApproval(undefined)).toBe(true);
  });
});

describe("activeRunOrigin", () => {
  it("recognizes active MCP and desktop runs", () => {
    expect(activeRunOrigin([run()])).toBe("mcp");
    expect(activeRunOrigin([run({ clientId: LOCAL_DESKTOP_RUN_CLIENT_ID })])).toBe("desktop");
    expect(activeRunOrigin([run({ clientId: LEGACY_DESKTOP_RUN_CLIENT_ID })])).toBe("other");
    expect(activeRunOrigin([run({ clientId: "unknown" })])).toBe("other");
  });

  it("ignores completed runs and returns no origin when idle", () => {
    expect(activeRunOrigin([run({ state: "completed" })])).toBeNull();
    expect(activeRunOrigin([])).toBeNull();
  });
});
