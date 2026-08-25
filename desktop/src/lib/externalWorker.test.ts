import { describe, expect, it } from "vitest";
import {
  applyExternalWorkerNotification,
  createExternalWorkerMonitor,
  EXTERNAL_WORKER_STREAMING_SUPPORTED,
  parseExternalWorkerArtifact,
  parseExternalWorkerEvent,
  parseExternalWorkerFollowUpRequest,
  parseExternalWorkerLaunchRequest,
  parseExternalWorkerLaunchResult,
  parseExternalWorkerNotification,
  parseExternalWorkerRecord,
} from "./externalWorker";

describe("external worker UI contract", () => {
  it("does not claim a sequenced provider stream", () => {
    expect(EXTERNAL_WORKER_STREAMING_SUPPORTED).toBe(false);
  });

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
      startingRef: "main\n",
      prompt: "Review",
      executionMode: "isolated",
      autoCreatePr: false,
    })).toBeNull();
  });

  it("refuses every autoCreatePr value except false", () => {
    const base = {
      requestId: "req-1",
      provider: "cursor_cloud",
      repository: "chriscase/GrokPtah",
      startingRef: "refs/heads/codex/review",
      prompt: "Review the exact candidate",
      executionMode: "isolated",
    };
    // false is the only accepted value: promotion stays a separate approval.
    expect(parseExternalWorkerLaunchRequest({ ...base, autoCreatePr: false })).not.toBeNull();
    // Asking the provider to open a pull request is refused outright.
    expect(parseExternalWorkerLaunchRequest({ ...base, autoCreatePr: true })).toBeNull();
    // Nothing truthy, absent, or loosely typed may stand in for false.
    for (const autoCreatePr of [null, undefined, 0, 1, "false", "true", {}, []]) {
      expect(
        parseExternalWorkerLaunchRequest({ ...base, autoCreatePr }),
        `autoCreatePr ${JSON.stringify(autoCreatePr) ?? "undefined"} must be refused`,
      ).toBeNull();
    }
    // Omitting the field entirely is refused too; the contract requires it.
    expect(parseExternalWorkerLaunchRequest(base)).toBeNull();
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
    expect(parseExternalWorkerArtifact({ path: "reports/review.json", digest: "sha256:abc", runId: "run-1" })).not.toBeNull();
    expect(parseExternalWorkerArtifact({ path: "../secret", digest: "sha256:abc", runId: "run-1" })).toBeNull();
    expect(parseExternalWorkerArtifact({ path: "reports/review.json", digest: "sha256:abc" })).toBeNull();
    expect(parseExternalWorkerArtifact({
      path: "reports/review.json",
      digest: "sha256:abc",
      runId: "run-1",
      url: "https://secret.example/file",
    })).toBeNull();
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
        lastSeq: null,
        stream: "unsupported",
        createdAt: "now",
        updatedAt: "now",
      },
    });
    expect(result?.run.externalRunId).toBe("run-1");
    expect(result?.run.stream).toBe("unsupported");
    expect(result?.run.lastSeq).toBeNull();
    expect(parseExternalWorkerLaunchResult({
      worker: result?.worker,
      run: { ...result?.run, lastSeq: 0 },
    })).toBeNull();
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
});
