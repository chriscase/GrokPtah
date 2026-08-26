import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it, vi } from "vitest";
import {
  type GrokPtahBrokerApprovalRequest,
  GrokPtahBrokerClient,
  GrokPtahBrokerError,
  parseBrokerApproval,
  parseBrokerBinding,
  parseBrokerEventUpdate,
  parseBrokerRun,
  parseBrokerReviewProjection,
  parseBrokerRunProjection,
} from "./grokptahBrokerClient";

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

describe("GrokPtahBrokerClient", () => {
  it("uses opaque broker ids and browser credentials without a bearer token", async () => {
    const fetcher = vi.fn<typeof fetch>().mockResolvedValue(
      jsonResponse({ brokerRunId: "run/1", bindingId: "binding/1" }),
    );
    const client = new GrokPtahBrokerClient({
      baseUrl: "https://contextdesk.example",
      fetcher,
      csrfToken: "csrf-1",
    });

    await client.submitRun(
      "binding/1",
      { prompt: "review", executionMode: "isolated_worktree" },
      "intent-1",
    );

    expect(String(fetcher.mock.calls[0][0])).toBe(
      "https://contextdesk.example/api/grokptah/v1/bindings/binding%2F1/runs",
    );
    expect(fetcher.mock.calls[0][1]).toMatchObject({
      credentials: "include",
      headers: {
        Accept: "application/json",
        "Idempotency-Key": "intent-1",
        "X-CSRF-Token": "csrf-1",
      },
    });
    expect(fetcher.mock.calls[0][1]?.headers).not.toHaveProperty("Authorization");
  });

  it("launches an isolated external worker through the broker without credentials", async () => {
    const fetcher = vi.fn<typeof fetch>().mockResolvedValue(jsonResponse({
      worker: {
        provider: "cursor_cloud",
        externalAgentId: "agent-1",
        repository: "org/repo",
        startingRef: "main",
        state: "running",
        createdAt: "2026-08-24T00:00:00Z",
        updatedAt: "2026-08-24T00:00:00Z",
      },
      run: {
        externalAgentId: "agent-1",
        externalRunId: "run-1",
        state: "running",
        lastSeq: 0,
        createdAt: "2026-08-24T00:00:00Z",
        updatedAt: "2026-08-24T00:00:00Z",
      },
    }));
    const client = new GrokPtahBrokerClient({
      baseUrl: "https://contextdesk.example",
      fetcher,
      csrfToken: "csrf-1",
    });
    const result = await client.launchExternalWorker("binding-1", {
      requestId: "request-1",
      provider: "cursor_cloud",
      repository: "org/repo",
      startingRef: "main",
      prompt: "Review the exact candidate",
      executionMode: "isolated",
      autoCreatePr: false,
    }, "request-1");
    expect(result.run.externalRunId).toBe("run-1");
    expect(String(fetcher.mock.calls[0][0])).toBe(
      "https://contextdesk.example/api/grokptah/v1/bindings/binding-1/external-workers",
    );
    expect(fetcher.mock.calls[0][1]?.headers).not.toHaveProperty("Authorization");
  });

  it("fails closed when a typed binding or run envelope is malformed", async () => {
    expect(parseBrokerBinding({
      bindingId: "binding-1",
      contract: "grokptah.broker.v1",
      expiresAt: "2026-08-24T00:00:00Z",
      capabilities: [{ id: "run.review", availability: "gated" }],
    })).not.toBeNull();
    expect(parseBrokerBinding({
      bindingId: "binding-1",
      contract: "grokptah.broker.v1",
      expiresAt: "2026-08-24T00:00:00Z",
      capabilities: [{ id: "run.review", availability: "gated" }, { id: "run.review", availability: "gated" }],
    })).toBeNull();
    expect(parseBrokerRun({ brokerRunId: "run-1", bindingId: "binding-1" })).not.toBeNull();
    expect(parseBrokerRun({ brokerRunId: "run-1", bindingId: "binding-1", workspace: "/secret" })).toBeNull();
    expect(parseBrokerApproval({
      approvalId: "approval-1",
      bindingId: "binding-1",
      brokerRunId: "run-1",
      sourceFingerprint: "source-1",
      finalFingerprint: "final-1",
      changedFiles: [{ path: "src/file.ts", summary: "bounded" }],
      expiresAt: "2026-08-24T23:00:00Z",
    })).not.toBeNull();
    expect(parseBrokerApproval({
      approvalId: "approval-1",
      bindingId: "binding-1",
      brokerRunId: "run-1",
      sourceFingerprint: "source-1",
      finalFingerprint: "final-1",
      changedFiles: [{ path: "..\\secret", summary: "bounded" }],
      expiresAt: "2026-08-24T23:00:00Z",
    })).toBeNull();
  });

  it("parses only bounded redacted run and review projections", async () => {
    const projection = parseBrokerRunProjection({
      brokerRunId: "run-1",
      bindingId: "binding-1",
      state: "running",
      promptPreview: "Review the staged change",
      createdAt: "2026-08-24T00:00:00Z",
      updatedAt: "2026-08-24T00:01:00Z",
      progress: {
        round: 2,
        maxRounds: 12,
        lastTool: "search",
        detail: "Inspecting the diff",
        updatedAt: "2026-08-24T00:01:00Z",
      },
      terminalResult: null,
      errorCode: null,
    });
    expect(projection?.progress?.round).toBe(2);
    expect(parseBrokerRunProjection({
      brokerRunId: "run-1",
      bindingId: "binding-1",
      state: "running",
      promptPreview: "Review",
      createdAt: "now",
      updatedAt: "now",
      workspace: "/private/secret",
    })).toBeNull();
    expect(parseBrokerRunProjection({
      brokerRunId: "run-1",
      bindingId: "binding-1",
      state: "running",
      promptPreview: "Review",
      createdAt: "now",
      updatedAt: "now",
      progress: { round: 25, maxRounds: 24, detail: "too far", updatedAt: "now" },
    })).toBeNull();
    expect(parseBrokerRunProjection({
      brokerRunId: "run-1",
      bindingId: "binding-1",
      state: "running",
      promptPreview: "Review /private/secret",
      createdAt: "now",
      updatedAt: "now",
    })).toBeNull();

    const review = parseBrokerReviewProjection({
      changedFiles: [{ path: "src/lib.ts", summary: "bounded" }],
      diff: "@@ -1 +1 @@",
      diffTruncated: false,
      fingerprint: "final-1",
    });
    expect(review?.changedFiles[0]?.path).toBe("src/lib.ts");
    expect(parseBrokerReviewProjection({
      changedFiles: [],
      diff: "diff",
      diffTruncated: false,
      fingerprint: "final-1",
      workspace: "/private/secret",
    })).toBeNull();
  });

  it("fails closed on unredacted broker event updates", () => {
    expect(parseBrokerEventUpdate({
      type: "progress",
      round: 2,
      maxRounds: 12,
      detail: "Inspecting the staged diff",
      updatedAt: "2026-08-24T00:01:00Z",
    })).toMatchObject({ type: "progress", round: 2 });
    expect(parseBrokerEventUpdate({ type: "progress", workspace: "/private/secret" })).toBeNull();
    expect(parseBrokerEventUpdate({ type: "progress", detail: "Authorization: Bearer secret" })).toBeNull();
    expect(parseBrokerEventUpdate({ type: "progress", round: 13, maxRounds: 12 })).toBeNull();
    expect(parseBrokerEventUpdate({})).toBeNull();
  });

  it("rejects malformed run responses before exposing them to a consumer", async () => {
    const fetcher = vi.fn<typeof fetch>().mockResolvedValue(jsonResponse({ brokerRunId: "run-1" }));
    const client = new GrokPtahBrokerClient({
      baseUrl: "https://contextdesk.example",
      fetcher,
      csrfToken: "csrf-1",
    });
    await expect(client.submitRun("binding-1", { prompt: "review" }, "intent-1"))
      .rejects.toMatchObject({ code: "invalid_response" });
  });

  it("rejects a well-formed run response bound to a different workspace", async () => {
    const fetcher = vi.fn<typeof fetch>().mockResolvedValue(
      jsonResponse({ brokerRunId: "run-1", bindingId: "other-binding" }),
    );
    const client = new GrokPtahBrokerClient({
      baseUrl: "https://contextdesk.example",
      fetcher,
      csrfToken: "csrf-1",
    });
    await expect(client.submitRun("binding-1", { prompt: "review" }, "intent-1"))
      .rejects.toMatchObject({ code: "invalid_response" });
  });

  it("binds typed run and review projections to the requested opaque scope", async () => {
    const fetcher = vi.fn<typeof fetch>()
      .mockResolvedValueOnce(jsonResponse({
        brokerRunId: "run-1",
        bindingId: "binding-1",
        state: "completed",
        promptPreview: "Review",
        createdAt: "now",
        updatedAt: "now",
        progress: null,
      }))
      .mockResolvedValueOnce(jsonResponse({
        changedFiles: [{ path: "src/lib.ts", summary: "bounded" }],
        diff: "diff",
        diffTruncated: false,
        fingerprint: "final-1",
      }));
    const client = new GrokPtahBrokerClient({ baseUrl: "https://contextdesk.example", fetcher });
    expect((await client.getRunProjection("binding-1", "run-1")).state).toBe("completed");
    expect((await client.getReviewProjection("binding-1", "run-1")).fingerprint).toBe("final-1");

    fetcher.mockResolvedValueOnce(jsonResponse({
      brokerRunId: "other-run",
      bindingId: "binding-1",
      state: "running",
      promptPreview: "Review",
      createdAt: "now",
      updatedAt: "now",
    }));
    await expect(client.getRunProjection("binding-1", "run-1"))
      .rejects.toMatchObject({ code: "invalid_response" });
  });

  it("rejects oversized broker JSON responses before exposing them", async () => {
    const fetcher = vi.fn<typeof fetch>().mockResolvedValue(
      new Response(JSON.stringify("x".repeat(4 * 1_048_576)), {
        status: 200,
        headers: { "content-type": "application/json" },
      }),
    );
    const client = new GrokPtahBrokerClient({ baseUrl: "https://contextdesk.example", fetcher });
    await expect(client.getRun("binding-1", "run-1"))
      .rejects.toMatchObject({ code: "invalid_response" });
  });

  it("validates binding responses before exposing capabilities", async () => {
    const fetcher = vi.fn<typeof fetch>().mockResolvedValue(jsonResponse({
      bindingId: "binding-1",
      contract: "grokptah.capabilities.v1",
      expiresAt: "2026-08-24T23:00:00Z",
      capabilities: [{ id: "run.review", availability: "available" }],
    }));
    const client = new GrokPtahBrokerClient({
      baseUrl: "https://contextdesk.example",
      fetcher,
      csrfToken: "csrf-1",
    });
    const binding = await client.createBinding(
      "war-room-1",
      "approved-workspace",
      ["run.review"],
      "bind-1",
    );
    expect(binding.capabilities[0]?.id).toBe("run.review");
  });

  it("rejects path-like binding aliases and malformed capability requests before transmission", async () => {
    const fetcher = vi.fn<typeof fetch>();
    const client = new GrokPtahBrokerClient({
      baseUrl: "https://contextdesk.example",
      fetcher,
      csrfToken: "csrf-1",
    });
    for (const [investigationId, workspace, capabilities] of [
      ["", "approved", ["run.review"]],
      ["war-room", "/Users/secret", ["run.review"]],
      ["war-room", "approved", ["run.review", "run.review"]],
      ["war-room", "approved", ["Run.Review"]],
    ] as const) {
      await expect(client.createBinding(investigationId, workspace, capabilities, "bind-1"))
        .rejects.toMatchObject({ code: "invalid_request" });
    }
    await expect(client.createBinding(" ", "approved", ["run.review"], "bind-1"))
      .rejects.toMatchObject({ code: "invalid_request" });
    await expect(client.createBinding("war-room", " ", ["run.review"], "bind-1"))
      .rejects.toMatchObject({ code: "invalid_request" });
    expect(fetcher).not.toHaveBeenCalled();
  });

  it("fails closed before a mutating request without a broker CSRF token", async () => {
    const fetcher = vi.fn<typeof fetch>();
    const client = new GrokPtahBrokerClient({ baseUrl: "https://contextdesk.example", fetcher });
    await expect(
      client.submitRun("binding-1", { prompt: "review" }, "intent-1"),
    ).rejects.toMatchObject<GrokPtahBrokerError>({ code: "csrf_required" });
    expect(fetcher).not.toHaveBeenCalled();
  });

  it("bounds CSRF configuration and oversized error bodies", async () => {
    expect(() => new GrokPtahBrokerClient({
      baseUrl: "https://contextdesk.example",
      csrfToken: "x".repeat(257),
    })).toThrowError(/CSRF token exceeds/);

    const fetcher = vi.fn<typeof fetch>().mockResolvedValue(
      new Response("x".repeat(64 * 1_024 + 1), { status: 500 }),
    );
    const client = new GrokPtahBrokerClient({ baseUrl: "https://contextdesk.example", fetcher });
    await expect(client.getRun("binding-1", "run-1"))
      .rejects.toMatchObject({ status: 500, code: "http_error" });
  });

  it("requires a non-empty idempotency key for every mutation", async () => {
    const fetcher = vi.fn<typeof fetch>();
    const client = new GrokPtahBrokerClient({
      baseUrl: "https://contextdesk.example",
      fetcher,
      csrfToken: "csrf-1",
    });
    await expect(client.submitRun("binding-1", { prompt: "review" }, "  ")).rejects.toMatchObject({
      code: "idempotency_required",
    });
    await expect(client.promoteRun("binding-1", "run-1", " ", "promote-intent-1"))
      .rejects.toMatchObject({ code: "invalid_request" });
    expect(fetcher).not.toHaveBeenCalled();
  });

  it("rejects malformed run bounds and prompts before transmission", async () => {
    const fetcher = vi.fn<typeof fetch>();
    const client = new GrokPtahBrokerClient({
      baseUrl: "https://contextdesk.example",
      fetcher,
      csrfToken: "csrf-1",
    });
    for (const request of [
      { prompt: "  " },
      { prompt: "review", executionMode: "desktop" },
      { prompt: "review", bounds: { maxRounds: 25 } },
      { prompt: "review", bounds: { maxDurationMs: 1.5 } },
      { prompt: "review", allowQueue: "yes" },
    ]) {
      await expect(client.submitRun("binding-1", request as never, "intent-1"))
        .rejects.toMatchObject({ code: "invalid_request" });
    }
    expect(fetcher).not.toHaveBeenCalled();
  });

  it("rejects empty queue and steer text before transmission", async () => {
    const fetcher = vi.fn<typeof fetch>();
    const client = new GrokPtahBrokerClient({
      baseUrl: "https://contextdesk.example",
      fetcher,
      csrfToken: "csrf-1",
    });
    await expect(client.queuePrompt("binding-1", "  ", "queue-1"))
      .rejects.toMatchObject({ code: "invalid_request" });
    await expect(client.steer("binding-1", "", "steer-1"))
      .rejects.toMatchObject({ code: "invalid_request" });
    expect(fetcher).not.toHaveBeenCalled();
  });

  it("fails closed before approval for malformed review evidence", async () => {
    const fetcher = vi.fn<typeof fetch>();
    const client = new GrokPtahBrokerClient({
      baseUrl: "https://contextdesk.example",
      fetcher,
      csrfToken: "csrf-1",
    });

    await expect(
      client.approveRun(
        "binding-1",
        "run-1",
        {
          sourceFingerprint: " ",
          finalFingerprint: "final-1",
          changedFiles: [],
          ttlMs: 0,
        },
        "approve-intent-1",
      ),
    ).rejects.toMatchObject({ code: "invalid_request" });
    expect(fetcher).not.toHaveBeenCalled();
  });

  it("validates changed-file path and UTF-8 summary bounds independently", async () => {
    const fetcher = vi.fn<typeof fetch>();
    const client = new GrokPtahBrokerClient({
      baseUrl: "https://contextdesk.example",
      fetcher,
      csrfToken: "csrf-1",
    });
    const base = {
      sourceFingerprint: "source-1",
      finalFingerprint: "final-1",
      ttlMs: 30_000,
    };

    for (const changedFiles of [
      [{ path: "../secret", summary: "bounded" }],
      [{ path: "/absolute", summary: "bounded" }],
      [{ path: "..\\secret", summary: "bounded" }],
      [{ path: "src/file.ts", summary: "é".repeat(257) }],
      [{ path: "src/file.ts", summary: "bounded", extra: true }],
    ]) {
      await expect(
        client.approveRun("binding-1", "run-1", { ...base, changedFiles }, "approve-intent-1"),
      ).rejects.toMatchObject({ code: "invalid_request" });
    }
    expect(fetcher).not.toHaveBeenCalled();
  });

  it("validates approval responses before exposing promotion evidence", async () => {
    const fetcher = vi.fn<typeof fetch>().mockResolvedValue(jsonResponse({
      approvalId: "approval-1",
      bindingId: "binding-1",
      brokerRunId: "run-1",
      sourceFingerprint: "source-1",
      finalFingerprint: "final-1",
      changedFiles: [{ path: "src/file.ts", summary: "bounded" }],
      expiresAt: "2026-08-24T23:00:00Z",
    }));
    const client = new GrokPtahBrokerClient({
      baseUrl: "https://contextdesk.example",
      fetcher,
      csrfToken: "csrf-1",
    });
    const approval = await client.approveRun(
      "binding-1",
      "run-1",
      { sourceFingerprint: "source-1", finalFingerprint: "final-1", changedFiles: [] },
      "approve-intent-1",
    );
    expect(approval.changedFiles[0]?.path).toBe("src/file.ts");

    fetcher.mockResolvedValueOnce(jsonResponse({
      approvalId: "approval-1",
      bindingId: "binding-1",
      brokerRunId: "run-1",
      sourceFingerprint: "source-1",
      finalFingerprint: "final-1",
      changedFiles: [{ path: "src/file.ts", summary: "bounded", extra: true }],
      expiresAt: "2026-08-24T23:00:00Z",
    }));
    await expect(
      client.approveRun(
        "binding-1",
        "run-1",
        { sourceFingerprint: "source-1", finalFingerprint: "final-1", changedFiles: [] },
        "approve-intent-2",
      ),
    ).rejects.toMatchObject({ code: "invalid_response" });

    fetcher.mockResolvedValueOnce(jsonResponse({
      approvalId: "approval-1",
      bindingId: "other-binding",
      brokerRunId: "run-1",
      sourceFingerprint: "source-1",
      finalFingerprint: "final-1",
      changedFiles: [],
      expiresAt: "2026-08-24T23:00:00Z",
    }));
    await expect(
      client.approveRun(
        "binding-1",
        "run-1",
        { sourceFingerprint: "source-1", finalFingerprint: "final-1", changedFiles: [] },
        "approve-intent-3",
      ),
    ).rejects.toMatchObject({ code: "invalid_response" });
  });

  it("validates approval TTL independently", async () => {
    const fetcher = vi.fn<typeof fetch>();
    const client = new GrokPtahBrokerClient({
      baseUrl: "https://contextdesk.example",
      fetcher,
      csrfToken: "csrf-1",
    });

    await expect(
      client.approveRun(
        "binding-1",
        "run-1",
        { sourceFingerprint: "source-1", finalFingerprint: "final-1", changedFiles: [], ttlMs: 0 },
        "approve-intent-1",
      ),
    ).rejects.toMatchObject({ code: "invalid_request" });
    expect(fetcher).not.toHaveBeenCalled();
  });

  it("fails closed for malformed changed-file runtime shapes", async () => {
    const fetcher = vi.fn<typeof fetch>();
    const client = new GrokPtahBrokerClient({
      baseUrl: "https://contextdesk.example",
      fetcher,
      csrfToken: "csrf-1",
    });
    const base = { sourceFingerprint: "source-1", finalFingerprint: "final-1", ttlMs: 30_000 };

    for (const changedFiles of [
      null,
      [{ path: 42, summary: "bounded" }],
      [{ path: "src/file.ts", summary: null }],
    ]) {
      await expect(
        client.approveRun(
          "binding-1",
          "run-1",
          { ...base, changedFiles } as unknown as GrokPtahBrokerApprovalRequest,
          "approve-intent-1",
        ),
      ).rejects.toMatchObject({ code: "invalid_request" });
    }
    expect(fetcher).not.toHaveBeenCalled();
  });

  it("fails closed before approval without CSRF", async () => {
    const fetcher = vi.fn<typeof fetch>();
    const client = new GrokPtahBrokerClient({
      baseUrl: "https://contextdesk.example",
      fetcher,
    });
    await expect(
      client.approveRun(
        "binding-1",
        "run-1",
        { sourceFingerprint: "source-1", finalFingerprint: "final-1", changedFiles: [] },
        "approve-intent-1",
      ),
    ).rejects.toMatchObject({ code: "csrf_required" });
    expect(fetcher).not.toHaveBeenCalled();
  });

  it("trims a broker CSRF token before sending it", async () => {
    const fetcher = vi.fn<typeof fetch>().mockResolvedValue(
      jsonResponse({ brokerRunId: "run-1", bindingId: "binding-1" }),
    );
    const client = new GrokPtahBrokerClient({
      baseUrl: "https://contextdesk.example",
      fetcher,
      csrfToken: "  csrf-1  ",
    });

    await client.submitRun("binding-1", { prompt: "review" }, "intent-1");
    expect(fetcher.mock.calls[0][1]?.headers).toMatchObject({ "X-CSRF-Token": "csrf-1" });
  });

  it("maps stable broker errors without exposing a privileged body", async () => {
    const fetcher = vi.fn<typeof fetch>().mockResolvedValue(
      jsonResponse(
        {
          code: "forbidden_scope",
          message: "workspace is not bound",
          requestId: "req-1",
          privilegedPath: "/Users/secret",
        },
        403,
      ),
    );
    const client = new GrokPtahBrokerClient({ baseUrl: "https://contextdesk.example", fetcher });

    await expect(client.getRun("binding-1", "run-1")).rejects.toEqual(
      expect.objectContaining<GrokPtahBrokerError>({
        status: 403,
        code: "forbidden_scope",
        requestId: "req-1",
      }),
    );
  });

  it("binds approval and promotion to opaque ids and broker CSRF", async () => {
    const fetcher = vi.fn<typeof fetch>()
      .mockResolvedValueOnce(jsonResponse({
        approvalId: "approval-1",
        bindingId: "binding/1",
        brokerRunId: "run/1",
        sourceFingerprint: "source-1",
        finalFingerprint: "final-1",
        changedFiles: [{ path: "src/lib.ts", summary: "Updated broker client contract" }],
        expiresAt: "2026-08-24T23:00:00Z",
      }))
      .mockResolvedValueOnce(jsonResponse({ promoted: true }));
    const client = new GrokPtahBrokerClient({
      baseUrl: "https://contextdesk.example",
      fetcher,
      csrfToken: "csrf-1",
    });

    await client.approveRun(
      "binding/1",
      "run/1",
      {
        sourceFingerprint: "source-1",
        finalFingerprint: "final-1",
        changedFiles: [{ path: "src/lib.ts", summary: "Updated broker client contract" }],
        ttlMs: 30_000,
      },
      "approve-intent-1",
    );
    await client.promoteRun("binding/1", "run/1", "approval/1", "promote-intent-1");

    expect(String(fetcher.mock.calls[0][0])).toBe(
      "https://contextdesk.example/api/grokptah/v1/bindings/binding%2F1/runs/run%2F1/approve",
    );
    expect(JSON.parse(String(fetcher.mock.calls[0][1]?.body))).toEqual({
      sourceFingerprint: "source-1",
      finalFingerprint: "final-1",
      changedFiles: [{ path: "src/lib.ts", summary: "Updated broker client contract" }],
      ttlMs: 30_000,
    });
    expect(String(fetcher.mock.calls[1][0])).toBe(
      "https://contextdesk.example/api/grokptah/v1/bindings/binding%2F1/runs/run%2F1/promote",
    );
    expect(JSON.parse(String(fetcher.mock.calls[1][1]?.body))).toEqual({
      approvalId: "approval/1",
    });
    expect(fetcher.mock.calls[1][1]?.headers).toMatchObject({
      "Idempotency-Key": "promote-intent-1",
      "X-CSRF-Token": "csrf-1",
    });
  });

  it("replays scoped broker events and stops at recovery", async () => {
    const stream = new ReadableStream<Uint8Array>({
      start(controller) {
        const text =
          'id: 4\ndata: {"kind":"event","brokerRunId":"run-1","seq":4,"ts":"now","update":{"type":"progress"}}\n\n' +
          'data: {"kind":"recovery","brokerRunId":"run-1","afterSeq":4,"reason":"cursor_expired","pollRoute":"/runs/run-1"}\n\n';
        controller.enqueue(new TextEncoder().encode(text));
        controller.close();
      },
    });
    const fetcher = vi.fn<typeof fetch>().mockResolvedValue(
      new Response(stream, { status: 200, headers: { "content-type": "text/event-stream" } }),
    );
    const client = new GrokPtahBrokerClient({ baseUrl: "https://contextdesk.example", fetcher });

    const notifications = [];
    for await (const notification of client.streamEvents("binding-1", "run-1")) {
      notifications.push(notification);
    }
    expect(notifications.map((notification) => notification.kind)).toEqual(["event", "recovery"]);
    expect(String(fetcher.mock.calls[0][0])).toContain("/bindings/binding-1/runs/run-1/events");
  });

  it("rejects privileged fields hidden inside an SSE event update", async () => {
    const stream = new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(new TextEncoder().encode(
          'id: 1\ndata: {"kind":"event","brokerRunId":"run-1","seq":1,"ts":"now","update":{"type":"progress","detail":"/Users/private/repo"}}\n\n',
        ));
        controller.close();
      },
    });
    const fetcher = vi.fn<typeof fetch>().mockResolvedValue(
      new Response(stream, { status: 200, headers: { "content-type": "text/event-stream" } }),
    );
    const client = new GrokPtahBrokerClient({ baseUrl: "https://contextdesk.example", fetcher });
    const read = async () => {
      for await (const _notification of client.streamEvents("binding-1", "run-1")) {
        // Privileged event data must never reach a browser consumer.
      }
    };
    await expect(read()).rejects.toThrow("malformed");
  });

  it("rejects an external recovery route", async () => {
    const stream = new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(new TextEncoder().encode(
          'data: {"kind":"recovery","brokerRunId":"run-1","afterSeq":4,"reason":"gap","pollRoute":"https://evil.example/recover"}\n\n',
        ));
        controller.close();
      },
    });
    const fetcher = vi.fn<typeof fetch>().mockResolvedValue(
      new Response(stream, { status: 200, headers: { "content-type": "text/event-stream" } }),
    );
    const client = new GrokPtahBrokerClient({ baseUrl: "https://contextdesk.example", fetcher });
    const read = async () => {
      for await (const _notification of client.streamEvents("binding-1", "run-1")) {
        // The invalid route must fail before yielding a recovery instruction.
      }
    };
    await expect(read()).rejects.toThrow("relative");
  });

  it("rejects a recovery cursor that moves behind observed events", async () => {
    const stream = new ReadableStream<Uint8Array>({
      start(controller) {
        const text =
          'id: 4\ndata: {"kind":"event","brokerRunId":"run-1","seq":4,"ts":"now","update":{"type":"progress"}}\n\n' +
          'data: {"kind":"recovery","brokerRunId":"run-1","afterSeq":3,"reason":"gap","pollRoute":"/runs/run-1"}\n\n';
        controller.enqueue(new TextEncoder().encode(text));
        controller.close();
      },
    });
    const fetcher = vi.fn<typeof fetch>().mockResolvedValue(
      new Response(stream, { status: 200, headers: { "content-type": "text/event-stream" } }),
    );
    const client = new GrokPtahBrokerClient({ baseUrl: "https://contextdesk.example", fetcher });
    const read = async () => {
      for await (const _notification of client.streamEvents("binding-1", "run-1")) {
        // A backwards recovery cursor must never reach a consumer.
      }
    };
    await expect(read()).rejects.toThrow("behind");
  });

  it("rejects malformed event sequence and timestamp metadata", async () => {
    const stream = new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(new TextEncoder().encode(
          'id: 1\ndata: {"kind":"event","brokerRunId":"run-1","seq":1.5,"ts":"now","update":{"type":"progress"}}\n\n',
        ));
        controller.close();
      },
    });
    const fetcher = vi.fn<typeof fetch>().mockResolvedValue(
      new Response(stream, { status: 200, headers: { "content-type": "text/event-stream" } }),
    );
    const client = new GrokPtahBrokerClient({ baseUrl: "https://contextdesk.example", fetcher });
    const read = async () => {
      for await (const _notification of client.streamEvents("binding-1", "run-1")) {
        // Malformed sequence metadata must never reach a consumer.
      }
    };
    await expect(read()).rejects.toThrow("malformed");
  });

  it("wraps non-JSON broker responses as safe invalid-response errors", async () => {
    const fetcher = vi.fn<typeof fetch>().mockResolvedValue(
      new Response("<html>proxy failure</html>", {
        status: 200,
        headers: { "content-type": "text/html" },
      }),
    );
    const client = new GrokPtahBrokerClient({ baseUrl: "https://contextdesk.example", fetcher });
    await expect(client.getRun("binding-1", "run-1")).rejects.toMatchObject({
      code: "invalid_response",
      status: 200,
    });
  });
});

describe("broker Help authority", () => {
  function clientWith(response: unknown, status = 200) {
    const calls: Array<{ url: string; init: RequestInit }> = [];
    const fetcher = (async (url: string, init: RequestInit) => {
      calls.push({ url, init });
      return new Response(JSON.stringify(response), {
        status,
        headers: { "Content-Type": "application/json" },
      });
    }) as unknown as typeof fetch;
    const client = new GrokPtahBrokerClient({
      baseUrl: "https://broker.example",
      fetcher,
      csrfToken: "csrf-token",
    });
    return { client, calls };
  }

  const decision = {
    allowed: true,
    allowedSourceIds: ["durable.lifecycle"],
    corpusDigest: "sha256:corpus",
    indexDigest: "sha256:index",
    receiptDigest: "sha256:receipt",
  };

  it("asks the broker for a decision rather than making one", async () => {
    const { client, calls } = clientWith(decision);
    const result = await client.authorizeHelp("binding-1", "search", "idem-1");
    expect(result.allowed).toBe(true);
    expect(result.allowedSourceIds).toEqual(["durable.lifecycle"]);
    expect(calls[0]!.url).toContain("/bindings/binding-1/help/authorize");
    expect(calls[0]!.init.method).toBe("POST");
  });

  it("refuses a decision payload that is not the closed shape", async () => {
    const { client } = clientWith({ allowed: true });
    await expect(client.authorizeHelp("binding-1", "search", "idem-1")).rejects.toThrow();
  });

  it("returns an answer receipt and nothing about the exchange", async () => {
    const receipt = {
      admissionId: "sha256:admission",
      requestDigest: "sha256:request",
      corpusDigest: "sha256:corpus",
      indexDigest: "sha256:index",
      outcome: "answered",
      outcomeDigest: "sha256:outcome",
      citedSourceIds: ["durable.lifecycle"],
      claimCount: 2,
    };
    const { client } = clientWith(receipt);
    const result = await client.answerHelp("binding-1", "durable run recovery", "idem-2");
    expect(result.outcome).toBe("answered");
    expect(result.claimCount).toBe(2);
    expect(JSON.stringify(result)).not.toContain("durable run recovery");
  });

  it("refuses a receipt carrying an artifact of the exchange", async () => {
    // A receipt is artifact-free by contract. One that is not did not come
    // from a broker holding that contract, and rendering it anyway would make
    // the client the place the guarantee breaks.
    const { client } = clientWith({
      admissionId: "sha256:admission",
      requestDigest: "sha256:request",
      corpusDigest: "sha256:corpus",
      indexDigest: "sha256:index",
      outcome: "answered",
      citedSourceIds: [],
      claimCount: 1,
      answer: "Resume freely after a restart.",
    });
    await expect(client.answerHelp("binding-1", "q", "idem-3")).rejects.toThrow();
  });

  it("requires a CSRF token and an idempotency key for both", async () => {
    const tokenless = new GrokPtahBrokerClient({
      baseUrl: "https://broker.example",
      fetcher: (async () => new Response("{}")) as unknown as typeof fetch,
    });
    await expect(tokenless.authorizeHelp("binding-1", "search", "idem")).rejects.toThrow();

    const { client } = clientWith(decision);
    await expect(client.authorizeHelp("binding-1", "search", "  ")).rejects.toThrow();
  });
});

describe("broker/Tauri receipt parity", () => {
  /**
   * The exact receipts the Rust executor emits.
   *
   * The desktop reaches the executor through a Tauri command and the browser
   * through the broker, but both hand the same receipt to a renderer. If the
   * Rust serialization and this parser disagreed about a field name, one of
   * those two paths would silently show nothing.
   *
   * Regenerate with
   * `cargo run -p grokptah-help-answer --example emit_receipt`.
   */
  const RECEIPTS = JSON.parse(
    readFileSync(
      resolve(
        dirname(fileURLToPath(import.meta.url)),
        "..", "..", "..",
        "crates", "common", "grokptah-help-answer", "fixtures", "receipt-shape.json",
      ),
      "utf8",
    ),
  ) as Array<Record<string, unknown>>;

  function clientReturning(body: unknown) {
    const fetcher = (async () =>
      new Response(JSON.stringify(body), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      })) as unknown as typeof fetch;
    return new GrokPtahBrokerClient({
      baseUrl: "https://broker.example",
      fetcher,
      csrfToken: "csrf-token",
    });
  }

  it("covers the outcomes the executor can produce", () => {
    expect(RECEIPTS.map((receipt) => receipt.outcome)).toEqual([
      "answered",
      "denied",
      "abandoned",
    ]);
  });

  it.each(RECEIPTS.map((receipt) => [String(receipt.outcome), receipt] as const))(
    "parses a %s receipt exactly as the executor emits it",
    async (outcome, receipt) => {
      const parsed = await clientReturning(receipt).answerHelp("binding-1", "q", "idem");
      expect(parsed.outcome).toBe(outcome);
      expect(parsed.admissionId).toBe(receipt.admissionId);
      expect(parsed.requestDigest).toBe(receipt.requestDigest);
      expect(parsed.corpusDigest).toBe(receipt.corpusDigest);
      expect(parsed.indexDigest).toBe(receipt.indexDigest);
      expect(parsed.claimCount).toBe(receipt.claimCount);
      expect(parsed.citedSourceIds).toEqual(receipt.citedSourceIds);
      // An omitted `failure` reads as "no failure", not as a parse error.
      expect(parsed.failure).toBe(receipt.failure ?? null);
      expect(parsed.outcomeDigest).toBe(receipt.outcomeDigest ?? null);
    },
  );

  it("refuses a receipt whose fields were renamed", async () => {
    // The failure this fixture exists to catch: a field renamed on one side.
    const renamed = { ...RECEIPTS[0], admission_id: RECEIPTS[0]!.admissionId };
    delete (renamed as Record<string, unknown>).admissionId;
    await expect(clientReturning(renamed).answerHelp("binding-1", "q", "idem")).rejects.toThrow();
  });
});
