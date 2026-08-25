import { describe, expect, it } from "vitest";
import {
  applyExternalWorkerNotification,
  createExternalWorkerMonitor,
  parseExternalWorkerArtifact,
  parseExternalWorkerEvent,
  parseExternalWorkerLaunchRequest,
  parseExternalWorkerLaunchResult,
  parseExternalWorkerNotification,
  parseExternalWorkerRecord,
} from "./externalWorker";

describe("external worker UI contract", () => {
  it("accepts exact isolated launches and rejects privileged or host-bound data", () => {
    expect(parseExternalWorkerLaunchRequest({
      requestId: "req-1",
      provider: "cursor_cloud",
      repository: "chriscase/GrokPtah",
      startingRef: "refs/heads/codex/review",
      prompt: "Review the exact candidate",
      executionMode: "isolated",
      autoCreatePr: false,
      bounds: { maxRounds: 8 },
    })?.startingRef).toBe("refs/heads/codex/review");
    expect(parseExternalWorkerLaunchRequest({
      requestId: "req-1",
      provider: "custom",
      repository: "/Users/secret/repo",
      startingRef: "main",
      prompt: "Review",
      executionMode: "isolated",
      autoCreatePr: false,
    })).toBeNull();
    expect(parseExternalWorkerLaunchRequest({
      requestId: "req-1",
      provider: "custom",
      providerId: "company-gateway",
      repository: "org/repo",
      startingRef: "main",
      prompt: "Review the exact candidate",
      executionMode: "isolated",
      autoCreatePr: false,
    })?.providerId).toBe("company-gateway");
  });

  it("parses redacted records and relative artifacts only", () => {
    expect(parseExternalWorkerRecord({
      provider: "cursor_cloud",
      externalAgentId: "agent-1",
      repository: "org/repo",
      startingRef: "main",
      state: "running",
      workerUrl: "https://cursor.com/agents/agent-1",
      createdAt: "2026-08-24T00:00:00Z",
      updatedAt: "2026-08-24T00:01:00Z",
    })?.state).toBe("running");
    expect(parseExternalWorkerRecord({
      provider: "cursor_cloud",
      externalAgentId: "agent-1",
      repository: "org/repo",
      startingRef: "main",
      state: "running",
      workerUrl: "file:///private/secret",
      createdAt: "now",
      updatedAt: "now",
    })).toBeNull();
    expect(parseExternalWorkerArtifact({ path: "reports/review.json", digest: "sha256:abc" })).not.toBeNull();
    expect(parseExternalWorkerArtifact({ path: "../secret", digest: "sha256:abc" })).toBeNull();
  });

  it("parses a launch envelope only when both worker and run projections are valid", () => {
    const result = parseExternalWorkerLaunchResult({
      worker: {
        provider: "cursor_cloud",
        externalAgentId: "agent-1",
        repository: "org/repo",
        startingRef: "main",
        state: "running",
        createdAt: "now",
        updatedAt: "now",
      },
      run: {
        externalAgentId: "agent-1",
        externalRunId: "run-1",
        state: "running",
        lastSeq: 0,
        createdAt: "now",
        updatedAt: "now",
      },
    });
    expect(result?.run.externalRunId).toBe("run-1");
    expect(parseExternalWorkerLaunchResult({ worker: result?.worker, run: { state: "running" } })).toBeNull();
  });

  it("requires cursor recovery instead of inferring completion", () => {
    const state = createExternalWorkerMonitor();
    const first = parseExternalWorkerNotification({
      type: "event",
      event: { seq: 0, ts: "2026-08-24T00:00:00Z", kind: "run.started", detail: "started" },
    });
    expect(first).not.toBeNull();
    const afterFirst = applyExternalWorkerNotification(state, first!);
    expect(afterFirst).toMatchObject({ lastSeq: 0, recoveryRequired: false });
    const gap = parseExternalWorkerEvent({ seq: 2, ts: "now", kind: "run.progress", detail: "checking" });
    expect(gap).not.toBeNull();
    const afterGap = applyExternalWorkerNotification(afterFirst!, { type: "event", event: gap! });
    expect(afterGap).toMatchObject({ lastSeq: 0, recoveryRequired: true });
    const recovery = parseExternalWorkerNotification({
      type: "recovery",
      afterSeq: 0,
      reason: "cursor_expired",
      pollRoute: "api/runs/run-1",
    });
    expect(recovery).not.toBeNull();
    expect(applyExternalWorkerNotification(afterFirst!, recovery!)).toMatchObject({ recoveryRequired: true });
    expect(parseExternalWorkerNotification({
      type: "event",
      event: { seq: 1, ts: "now", kind: "run.progress", detail: "Authorization: secret" },
    })).toBeNull();
  });
});
