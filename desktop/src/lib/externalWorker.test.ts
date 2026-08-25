import { describe, expect, it } from "vitest";
import schema from "../../../docs/schemas/grokptah-external-worker.v1.schema.json";
import {
  applyExternalWorkerNotification,
  createExternalWorkerMonitor,
  EXTERNAL_WORKER_CONTRACT,
  EXTERNAL_WORKER_STREAMING_SUPPORTED,
  MAX_EXTERNAL_WORKER_ARTIFACTS,
  MAX_EXTERNAL_WORKER_ARTIFACT_BYTES,
  parseExternalWorkerArtifact,
  parseExternalWorkerArtifactListing,
  parseExternalWorkerEvent,
  parseExternalWorkerFollowUpRequest,
  parseExternalWorkerLaunchRequest,
  parseExternalWorkerLaunchResult,
  parseExternalWorkerNotification,
  parseExternalWorkerRecord,
} from "./externalWorker";

const SHA256_DIGEST_PATTERN = "^sha256:[0-9a-f]{64}$";
const DIGEST = "sha256:be426b4d0bc6e0536d2bb2e8917792b442ac93cfa0ea7ff26a95e00b62a5af37";

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
    expect(parseExternalWorkerArtifact({ path: "reports/review.json", digest: DIGEST, runId: "run-1" })).not.toBeNull();
    expect(parseExternalWorkerArtifact({ path: "../secret", digest: DIGEST, runId: "run-1" })).toBeNull();
    expect(parseExternalWorkerArtifact({ path: "reports/review.json", digest: DIGEST })).toBeNull();
    expect(parseExternalWorkerArtifact({
      path: "reports/review.json",
      digest: DIGEST,
      runId: "run-1",
      url: "https://secret.example/file",
    })).toBeNull();
  });

  it("refuses artifact digests that are not a full SHA-256", () => {
    expect(parseExternalWorkerArtifact({ path: "artifacts/review.json", digest: DIGEST, runId: "run-1" })).not.toBeNull();
    for (const digest of [
      "sha256:abc",
      "trust-me",
      "md5:9f86d081884c7d659a2feaa0c55ad015",
      DIGEST.toUpperCase(),
      `${DIGEST}0`,
      DIGEST.slice(0, -1),
      DIGEST.replace("sha256", "sha512"),
    ]) {
      expect(
        parseExternalWorkerArtifact({ path: "artifacts/review.json", digest, runId: "run-1" }),
        `digest ${digest} must be refused`,
      ).toBeNull();
    }
  });

  it("refuses every absolute, traversing, or ambiguous artifact path", () => {
    for (const path of [
      "C:/Windows/System32/config",
      "c:/Users/secret/.ssh/id_ed25519",
      "/etc/passwd",
      "~/.ssh/id_ed25519",
      "artifacts/../../etc/passwd",
      "artifacts//review.json",
      "artifacts/./review.json",
      "artifacts/",
      "artifacts/review.json?sig=secret",
      "artifacts/review.json#fragment",
      "artifacts\\review.json",
    ]) {
      expect(
        parseExternalWorkerArtifact({ path, digest: DIGEST, runId: "run-1" }),
        `path ${path} must be refused`,
      ).toBeNull();
    }
  });

  it("bounds artifact size and listing length, and enforces attribution", () => {
    const artifact = { path: "artifacts/review.json", digest: DIGEST, runId: "run-1" };
    expect(parseExternalWorkerArtifact({ ...artifact, sizeBytes: MAX_EXTERNAL_WORKER_ARTIFACT_BYTES })).not.toBeNull();
    expect(parseExternalWorkerArtifact({ ...artifact, sizeBytes: MAX_EXTERNAL_WORKER_ARTIFACT_BYTES + 1 })).toBeNull();
    expect(parseExternalWorkerArtifact({ ...artifact, sizeBytes: Number.MAX_SAFE_INTEGER })).toBeNull();

    expect(parseExternalWorkerArtifactListing([artifact], "run-1")).toHaveLength(1);
    // Attribution belongs to the listing, not to one artifact.
    expect(parseExternalWorkerArtifactListing([artifact], "run-2")).toBeNull();
    expect(parseExternalWorkerArtifactListing(
      Array.from({ length: MAX_EXTERNAL_WORKER_ARTIFACTS }, () => artifact),
      "run-1",
    )).toHaveLength(MAX_EXTERNAL_WORKER_ARTIFACTS);
    expect(parseExternalWorkerArtifactListing(
      Array.from({ length: MAX_EXTERNAL_WORKER_ARTIFACTS + 1 }, () => artifact),
      "run-1",
    )).toBeNull();
    // One bad member fails the whole listing closed.
    expect(parseExternalWorkerArtifactListing([artifact, { ...artifact, digest: "sha256:abc" }], "run-1")).toBeNull();
    expect(parseExternalWorkerArtifactListing("not-an-array", "run-1")).toBeNull();
  });

  it("agrees with the published v1 schema on artifact bounds", () => {
    const artifact = schema.$defs.artifact.properties;
    expect(schema.$defs.digest.pattern).toBe(SHA256_DIGEST_PATTERN);
    expect(artifact.sizeBytes.maximum).toBe(MAX_EXTERNAL_WORKER_ARTIFACT_BYTES);
    expect(schema.properties.artifacts.maxItems).toBe(MAX_EXTERNAL_WORKER_ARTIFACTS);
    // The artifact must not fall back to the looser `ref` and `identity` rules.
    expect(artifact.path.$ref).toBe("#/$defs/artifactPath");
    expect(artifact.digest.$ref).toBe("#/$defs/digest");
    expect(schema.properties.contract.const).toBe(EXTERNAL_WORKER_CONTRACT);

    // The schema's own path rule must accept and refuse exactly what the
    // parser does, so a non-TypeScript consumer implementing the schema lands
    // on the same containment rule.
    const schemaPath = new RegExp(schema.$defs.artifactPath.pattern, "u");
    for (const path of [
      "artifacts/report.md",
      "artifacts/a/b/c.json",
      "artifacts/..hidden",
    ]) {
      expect(schemaPath.test(path), `schema must accept ${path}`).toBe(true);
      expect(parseExternalWorkerArtifact({ path, digest: DIGEST, runId: "run-1" })).not.toBeNull();
    }
    for (const path of [
      "C:/Windows/System32/config",
      "/etc/passwd",
      "~/.ssh/id_ed25519",
      "artifacts/../../etc/passwd",
      "artifacts//review.json",
      "artifacts/./review.json",
      "artifacts/",
      "artifacts/review.json?sig=secret",
      "artifacts/review.json#fragment",
      "artifacts\\review.json",
    ]) {
      expect(schemaPath.test(path), `schema must refuse ${path}`).toBe(false);
      expect(parseExternalWorkerArtifact({ path, digest: DIGEST, runId: "run-1" })).toBeNull();
    }
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
