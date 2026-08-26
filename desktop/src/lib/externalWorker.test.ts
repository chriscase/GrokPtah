import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";
import {
  applyExternalWorkerNotification,
  createExternalWorkerMonitor,
  externalWorkerAdmissionCovers,
  externalWorkerCapabilityAvailable,
  externalWorkerReceiptBlocksRetry,
  isExternalWorkerAdmissionLive,
  parseExternalWorkerAdmission,
  parseExternalWorkerArtifact,
  parseExternalWorkerCapabilityStatus,
  parseExternalWorkerEvent,
  parseExternalWorkerFollowUpRequest,
  parseExternalWorkerLaunchRequest,
  parseExternalWorkerLaunchResult,
  parseExternalWorkerNotification,
  parseExternalWorkerReceipt,
  parseExternalWorkerRecord,
  EXTERNAL_WORKER_CONTRACT,
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

describe("external worker production authority projections", () => {
  const DIGEST = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
  const scope = {
    principalId: "principal-1",
    sessionId: "session-1",
    workspace: "grokptah-main",
    runId: "run-1",
  };

  function admission(overrides: Record<string, unknown> = {}) {
    return {
      contract: EXTERNAL_WORKER_CONTRACT,
      admissionId: "adm-1",
      nonce: "nonce-1",
      requestId: "request-1",
      scope,
      mutation: "launch",
      provider: "cursor_cloud",
      capabilityRevision: 1,
      issuedAtMs: 1_000,
      expiresAtMs: 61_000,
      payloadDigest: DIGEST,
      ...overrides,
    };
  }

  function receipt(overrides: Record<string, unknown> = {}) {
    return {
      contract: EXTERNAL_WORKER_CONTRACT,
      requestId: "request-1",
      admissionId: "adm-1",
      mutation: "launch",
      scope,
      provider: "cursor_cloud",
      providerRequestId: "ewp-stable",
      attempt: 1,
      state: "accepted",
      target: { externalAgentId: "agent-1", externalRunId: "run-1" },
      payloadDigest: DIGEST,
      reason: "provider accepted the admitted mutation",
      createdAtMs: 1_000,
      updatedAtMs: 2_000,
      ...overrides,
    };
  }

  it("accepts a well-formed admission and enforces its exact lifetime", () => {
    const parsed = parseExternalWorkerAdmission(admission());
    expect(parsed).not.toBeNull();
    expect(isExternalWorkerAdmissionLive(parsed!, 999)).toBe(false);
    expect(isExternalWorkerAdmissionLive(parsed!, 1_000)).toBe(true);
    expect(isExternalWorkerAdmissionLive(parsed!, 60_999)).toBe(true);
    expect(isExternalWorkerAdmissionLive(parsed!, 61_000)).toBe(false);
  });

  it("fails closed on malformed, stale, over-scoped, or path-bearing admissions", () => {
    const rejected: Array<[string, Record<string, unknown>]> = [
      ["foreign contract", { contract: "grokptah.external-workers.v2" }],
      ["unknown key", { providerUrl: "https://api.cursor.com" }],
      ["bad digest", { payloadDigest: "sha1:abc" }],
      ["uppercase digest", { payloadDigest: "sha256:" + "A".repeat(64) }],
      ["inverted lifetime", { issuedAtMs: 61_000, expiresAtMs: 1_000 }],
      ["zero lifetime", { issuedAtMs: 1_000, expiresAtMs: 1_000 }],
      ["unbounded lifetime", { issuedAtMs: 0, expiresAtMs: 16 * 60 * 1_000 }],
      ["unknown mutation", { mutation: "promote" }],
      ["unknown provider", { provider: "acme_cloud" }],
      ["custom without identity", { provider: "custom" }],
      ["launch with a target", { target: { externalAgentId: "agent-1" } }],
      ["follow-up without a target", { mutation: "follow_up" }],
      [
        "follow-up naming a run",
        { mutation: "follow_up", target: { externalAgentId: "a", externalRunId: "r" } },
      ],
      ["cancel without a run", { mutation: "cancel", target: { externalAgentId: "a" } }],
      ["absolute workspace", { scope: { ...scope, workspace: "/Users/dev/GrokPtah" } }],
      ["windows workspace", { scope: { ...scope, workspace: "C:\\Users\\dev" } }],
      ["traversal workspace", { scope: { ...scope, workspace: "../escape" } }],
      ["empty principal", { scope: { ...scope, principalId: "" } }],
      ["negative revision", { capabilityRevision: -1 }],
    ];
    for (const [label, overrides] of rejected) {
      expect(parseExternalWorkerAdmission(admission(overrides)), label).toBeNull();
    }
  });

  it("accepts a redacted receipt and refuses one carrying privileged text", () => {
    expect(parseExternalWorkerReceipt(receipt())).not.toBeNull();
    const rejected: Array<[string, Record<string, unknown>]> = [
      ["bearer token", { reason: "provider said Authorization: Bearer abc" }],
      ["provider url", { reason: "see https://api.cursor.com/v1/agents" }],
      ["host path", { reason: "wrote /Users/dev/GrokPtah/out.json" }],
      ["api key", { reason: "api_key rotated" }],
      ["empty reason", { reason: "" }],
      ["zero attempt", { attempt: 0 }],
      ["unknown state", { state: "settled" }],
      ["accepted without target", { target: undefined }],
      ["backwards clock", { createdAtMs: 5_000, updatedAtMs: 1_000 }],
      ["unknown key", { apiKey: "leak" }],
      ["absolute workspace", { scope: { ...scope, workspace: "/srv/repo" } }],
    ];
    for (const [label, overrides] of rejected) {
      expect(parseExternalWorkerReceipt(receipt(overrides)), label).toBeNull();
    }
  });

  it("treats every non-rejected receipt state as retry-blocking", () => {
    expect(externalWorkerReceiptBlocksRetry("claimed")).toBe(true);
    expect(externalWorkerReceiptBlocksRetry("uncertain")).toBe(true);
    expect(externalWorkerReceiptBlocksRetry("accepted")).toBe(true);
    expect(externalWorkerReceiptBlocksRetry("rejected")).toBe(false);
    expect(parseExternalWorkerReceipt(receipt({ state: "uncertain", target: undefined })))
      .not.toBeNull();
  });

  it("advertises a capability only when every gate holds", () => {
    const base = {
      provider: "cursor_cloud" as const,
      registered: true,
      reachable: true,
      versionCompatible: true,
      policyAllowed: true,
      capabilityRevision: 2,
    };
    const available = parseExternalWorkerCapabilityStatus(base);
    expect(available).not.toBeNull();
    expect(externalWorkerCapabilityAvailable(available!)).toBe(true);

    for (const gate of ["registered", "reachable", "versionCompatible", "policyAllowed"] as const) {
      expect(
        parseExternalWorkerCapabilityStatus({ ...base, [gate]: false }),
        `${gate} must require a reason`,
      ).toBeNull();
      const withReason = parseExternalWorkerCapabilityStatus({
        ...base,
        [gate]: false,
        reason: "adapter gate is not satisfied",
      });
      expect(withReason).not.toBeNull();
      expect(externalWorkerCapabilityAvailable(withReason!)).toBe(false);
    }
    expect(
      parseExternalWorkerCapabilityStatus({ ...base, reason: "see https://api.cursor.com" }),
    ).toBeNull();
  });

  it("only lets an admission cover its exact mutation, scope, and target", () => {
    const launch = parseExternalWorkerAdmission(admission())!;
    expect(externalWorkerAdmissionCovers(launch, "launch", scope, "request-1")).toBe(true);
    expect(externalWorkerAdmissionCovers(launch, "cancel", scope, "request-1")).toBe(false);
    expect(externalWorkerAdmissionCovers(launch, "launch", scope, "request-2")).toBe(false);
    expect(
      externalWorkerAdmissionCovers(
        launch,
        "launch",
        { ...scope, principalId: "principal-2" },
        "request-1",
      ),
    ).toBe(false);

    const cancel = parseExternalWorkerAdmission(
      admission({
        mutation: "cancel",
        target: { externalAgentId: "agent-1", externalRunId: "run-a" },
      }),
    )!;
    expect(
      externalWorkerAdmissionCovers(cancel, "cancel", scope, "request-1", {
        externalAgentId: "agent-1",
        externalRunId: "run-a",
      }),
    ).toBe(true);
    expect(
      externalWorkerAdmissionCovers(cancel, "cancel", scope, "request-1", {
        externalAgentId: "agent-1",
        externalRunId: "run-b",
      }),
    ).toBe(false);
    expect(externalWorkerAdmissionCovers(cancel, "cancel", scope, "request-1")).toBe(false);
  });

  it("matches the published v1 projection schema for admissions and receipts", () => {
    const schema = JSON.parse(
      readFileSync(
        resolve(process.cwd(), "../docs/schemas/grokptah-external-worker.v1.schema.json"),
        "utf8",
      ),
    ) as {
      $defs: Record<string, { required?: string[]; properties?: Record<string, unknown> }>;
    };
    const admissionSchema = schema.$defs.admission;
    const receiptSchema = schema.$defs.receipt;
    expect(admissionSchema).toBeDefined();
    expect(receiptSchema).toBeDefined();

    const admissionValue = admission() as Record<string, unknown>;
    for (const key of admissionSchema.required ?? []) {
      expect(admissionValue, `admission is missing ${key}`).toHaveProperty(key);
    }
    for (const key of Object.keys(admissionValue)) {
      expect(admissionSchema.properties, `schema is missing ${key}`).toHaveProperty(key);
    }

    const receiptValue = receipt() as Record<string, unknown>;
    for (const key of receiptSchema.required ?? []) {
      expect(receiptValue, `receipt is missing ${key}`).toHaveProperty(key);
    }
    for (const key of Object.keys(receiptValue)) {
      expect(receiptSchema.properties, `schema is missing ${key}`).toHaveProperty(key);
    }
  });
});
