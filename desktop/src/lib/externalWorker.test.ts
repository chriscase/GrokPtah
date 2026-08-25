import { describe, expect, it } from "vitest";
import {
  applyExternalWorkerNotification,
  createExternalWorkerMonitor,
  parseExternalWorkerArtifact,
  parseExternalWorkerEvent,
  parseExternalWorkerFollowUpRequest,
  parseExternalWorkerLaunchRequest,
  parseExternalWorkerLaunchResult,
  parseExternalWorkerListPage,
  parseExternalWorkerListQuery,
  parseExternalWorkerNotification,
  parseExternalWorkerRecord,
  parseExternalWorkerSummary,
  replaceExternalWorkerMonitor,
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
    expect(parseExternalWorkerLaunchRequest({
      requestId: "req-1",
      provider: "cursor_cloud",
      repository: "org/repo",
      startingRef: "main",
      prompt: "Review",
      executionMode: "isolated",
      autoCreatePr: true,
    })).toBeNull();
    expect(parseExternalWorkerLaunchRequest({
      requestId: "req-1",
      provider: "cursor_cloud",
      repository: "org/repo",
      startingRef: "main",
      prompt: "Review\u0007",
      executionMode: "isolated",
      autoCreatePr: false,
    })).toBeNull();
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
    expect(parseExternalWorkerRecord({
      provider: "cursor_cloud",
      externalAgentId: "agent-1",
      repository: "org/repo",
      startingRef: "main",
      state: "running",
      workerUrl: "https://cursor.com/agents/agent-1?token=secret",
      createdAt: "now",
      updatedAt: "now",
    })).toBeNull();
    expect(parseExternalWorkerArtifact({ path: "reports/review.json", digest: "sha256:abc" })).not.toBeNull();
    expect(parseExternalWorkerArtifact({ path: "../secret", digest: "sha256:abc" })).toBeNull();
  });

  it("accepts bounded follow-ups but rejects empty prompts and unknown fields", () => {
    expect(parseExternalWorkerFollowUpRequest({
      requestId: "follow-up-1",
      prompt: "Re-check the focused change",
      bounds: { maxRounds: 8 },
    })?.requestId).toBe("follow-up-1");
    expect(parseExternalWorkerFollowUpRequest({
      requestId: "follow-up-1",
      prompt: "",
    })).toBeNull();
    expect(parseExternalWorkerFollowUpRequest({
      requestId: "follow-up-1",
      prompt: "Re-check",
      unexpected: true,
    })).toBeNull();
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
    expect(parseExternalWorkerLaunchResult({
      worker: result?.worker,
      run: { ...result?.run, externalAgentId: "other-agent" },
    })).toBeNull();
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
    const lateContiguous = parseExternalWorkerEvent({ seq: 1, ts: "now", kind: "run.progress", detail: "late" });
    expect(lateContiguous).not.toBeNull();
    expect(applyExternalWorkerNotification(afterGap!, { type: "event", event: lateContiguous! }))
      .toMatchObject({ lastSeq: 1, recoveryRequired: true });
    const recovery = parseExternalWorkerNotification({
      type: "recovery",
      afterSeq: 0,
      reason: "cursor_expired",
      pollRoute: "/api/runs/run-1",
    });
    expect(recovery).not.toBeNull();
    expect(applyExternalWorkerNotification(afterFirst!, recovery!)).toMatchObject({ recoveryRequired: true });
    expect(parseExternalWorkerNotification({
      type: "recovery",
      afterSeq: 0,
      reason: "cursor_expired",
      pollRoute: "//evil.example/runs/run-1",
    })).toBeNull();
    expect(parseExternalWorkerNotification({
      type: "event",
      event: { seq: 1, ts: "now", kind: "run.progress", detail: "Authorization: secret" },
    })).toBeNull();
  });

  it("clears recovery only from a contiguous authoritative snapshot", () => {
    expect(replaceExternalWorkerMonitor([
      { seq: 8, ts: "now", kind: "run.progress", detail: "replayed" },
      { seq: 9, ts: "now", kind: "run.completed", detail: "done" },
    ])).toMatchObject({ lastSeq: 9, recoveryRequired: false });
    expect(replaceExternalWorkerMonitor([
      { seq: 8, ts: "now", kind: "run.progress", detail: "replayed" },
      { seq: 10, ts: "now", kind: "run.completed", detail: "gap" },
    ])).toBeNull();
    expect(replaceExternalWorkerMonitor([
      { seq: 8, ts: "now", kind: "run.progress", detail: "Authorization: secret" },
    ])).toBeNull();
    expect(replaceExternalWorkerMonitor(
      Array.from({ length: 257 }, (_, seq) => ({ seq, ts: "now", kind: "run.progress", detail: "bounded" })),
    )).toBeNull();
  });

  it("parses identity-only list pages and rejects privileged or unknown fields", () => {
    expect(parseExternalWorkerListQuery({
      limit: 20,
      includeArchived: false,
    })).toEqual({ limit: 20, includeArchived: false });
    expect(parseExternalWorkerListQuery({})).toEqual({ includeArchived: false });
    expect(parseExternalWorkerListQuery({ limit: 20 })).toEqual({
      limit: 20,
      includeArchived: false,
    });
    expect(parseExternalWorkerListQuery({ includeArchived: true })).toEqual({
      includeArchived: true,
    });
    expect(parseExternalWorkerListQuery({ includeArchived: null })).toBeNull();
    expect(parseExternalWorkerListQuery({ limit: 0 })).toBeNull();
    expect(parseExternalWorkerListQuery({ limit: 101 })).toBeNull();
    expect(parseExternalWorkerListQuery({
      prUrl: "https://github.com/org/repo/pull/1",
    })).toBeNull();
    expect(parseExternalWorkerListQuery({ cursor: "page\n2" })).toBeNull();

    const page = parseExternalWorkerListPage({
      items: [{
        provider: "cursor_cloud",
        externalAgentId: "agent-1",
        state: "ready",
        workerUrl: "https://cursor.com/agents/agent-1",
        latestRunId: "run-1",
        createdAt: "now",
        updatedAt: "now",
      }],
      nextCursor: "agent-2",
    });
    expect(page?.items[0]?.externalAgentId).toBe("agent-1");
    expect(page?.nextCursor).toBe("agent-2");
    expect(parseExternalWorkerSummary({
      provider: "cursor_cloud",
      externalAgentId: "agent-1",
      repository: "org/repo",
      startingRef: "main",
      state: "ready",
      createdAt: "now",
      updatedAt: "now",
    })).toBeNull();
    expect(parseExternalWorkerListPage({
      items: [{
        provider: "cursor_cloud",
        externalAgentId: "agent-1",
        state: "ready",
        workerUrl: "https://cursor.com/agents/agent-1?token=secret",
        createdAt: "now",
        updatedAt: "now",
      }],
    })).toBeNull();
    expect(parseExternalWorkerListPage({
      items: [],
      nextCursor: "agent-2",
    })).toBeNull();
    expect(parseExternalWorkerListPage({
      items: [{
        provider: "cursor_cloud",
        externalAgentId: "agent-1",
        state: "ready",
        createdAt: "now",
        updatedAt: "now",
      }, {
        provider: "cursor_cloud",
        externalAgentId: "agent-1",
        state: "archived",
        createdAt: "now",
        updatedAt: "now",
      }],
    })).toBeNull();
    expect(parseExternalWorkerListPage({
      items: [],
      rawProvider: { authorization: "Bearer secret" },
    })).toBeNull();
  });
});
