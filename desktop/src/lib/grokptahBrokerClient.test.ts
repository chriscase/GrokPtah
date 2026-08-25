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

  it("rejects external worker responses whose identities do not match the route", async () => {
    const fetcher = vi.fn<typeof fetch>()
      .mockResolvedValueOnce(jsonResponse({
        provider: "cursor_cloud",
        externalAgentId: "other-agent",
        repository: "org/repo",
        startingRef: "main",
        state: "running",
        createdAt: "now",
        updatedAt: "now",
      }))
      .mockResolvedValueOnce(jsonResponse({
        externalAgentId: "agent-1",
        externalRunId: "other-run",
        state: "running",
        lastSeq: 0,
        createdAt: "now",
        updatedAt: "now",
      }));
    const client = new GrokPtahBrokerClient({
      baseUrl: "https://contextdesk.example",
      fetcher,
    });

    await expect(client.getExternalWorker("binding-1", "agent-1"))
      .rejects.toMatchObject({ code: "invalid_response" });
    await expect(client.getExternalWorkerRun("binding-1", "agent-1", "run-1"))
      .rejects.toMatchObject({ code: "invalid_response" });
  });

  it("rejects a launch response from a different provider profile", async () => {
    const fetcher = vi.fn<typeof fetch>().mockResolvedValue(jsonResponse({
      worker: {
        provider: "custom",
        providerId: "other-gateway",
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
    }));
    const client = new GrokPtahBrokerClient({
      baseUrl: "https://contextdesk.example",
      fetcher,
      csrfToken: "csrf-1",
    });
    await expect(client.launchExternalWorker("binding-1", {
      requestId: "request-1",
      provider: "cursor_cloud",
      repository: "org/repo",
      startingRef: "main",
      prompt: "Review the exact candidate",
      executionMode: "isolated",
      autoCreatePr: false,
    }, "request-1")).rejects.toMatchObject({ code: "invalid_response" });
  });

  it("lists and archives external workers without credentials or implied cancel", async () => {
    const fetcher = vi.fn<typeof fetch>()
      .mockResolvedValueOnce(jsonResponse({
        items: [{
          provider: "cursor_cloud",
          externalAgentId: "agent-1",
          state: "ready",
          createdAt: "now",
          updatedAt: "now",
        }],
        nextCursor: "agent-2",
      }))
      .mockResolvedValueOnce(jsonResponse({
        items: [{
          provider: "cursor_cloud",
          externalAgentId: "agent-1",
          state: "ready",
          createdAt: "now",
          updatedAt: "now",
        }],
      }))
      .mockResolvedValueOnce(jsonResponse({
        provider: "cursor_cloud",
        externalAgentId: "agent-1",
        repository: "org/repo",
        startingRef: "main",
        state: "archived",
        createdAt: "now",
        updatedAt: "now",
      }))
      .mockResolvedValueOnce(jsonResponse({
        provider: "cursor_cloud",
        externalAgentId: "agent-1",
        repository: "org/repo",
        startingRef: "main",
        state: "ready",
        createdAt: "now",
        updatedAt: "now",
      }))
      .mockResolvedValueOnce(jsonResponse({
        externalAgentId: "agent-1",
        externalRunId: "run-1",
        state: "cancelled",
        lastSeq: 0,
        createdAt: "now",
        updatedAt: "now",
      }));
    const client = new GrokPtahBrokerClient({
      baseUrl: "https://contextdesk.example",
      fetcher,
      csrfToken: "csrf-1",
    });

    const page = await client.listExternalWorkers("binding-1", { limit: 1, includeArchived: false });
    expect(page.items[0]?.externalAgentId).toBe("agent-1");
    expect(String(fetcher.mock.calls[0][0])).toBe(
      "https://contextdesk.example/api/grokptah/v1/bindings/binding-1/external-workers?limit=1&includeArchived=false",
    );
    expect(fetcher.mock.calls[0][1]?.method ?? "GET").toBe("GET");
    expect(fetcher.mock.calls[0][1]?.headers).not.toHaveProperty("Authorization");
    expect(fetcher.mock.calls[0][1]?.headers).not.toHaveProperty("X-CSRF-Token");
    expect(fetcher.mock.calls[0][1]?.headers).not.toHaveProperty("Idempotency-Key");

    const omitted = await client.listExternalWorkers("binding-1");
    expect(omitted.items).toHaveLength(1);
    expect(String(fetcher.mock.calls[1][0])).toBe(
      "https://contextdesk.example/api/grokptah/v1/bindings/binding-1/external-workers?includeArchived=false",
    );

    const archived = await client.archiveExternalWorker("binding-1", "agent-1", "archive-1");
    expect(archived.state).toBe("archived");
    expect(String(fetcher.mock.calls[2][0])).toContain("/external-workers/agent-1/archive");
    expect(fetcher.mock.calls[2][1]).toMatchObject({
      method: "POST",
      headers: {
        "Idempotency-Key": "archive-1",
        "X-CSRF-Token": "csrf-1",
      },
    });
    expect(fetcher.mock.calls[2][1]?.body).toBeUndefined();

    const restored = await client.unarchiveExternalWorker("binding-1", "agent-1", "unarchive-1");
    expect(restored.state).toBe("ready");
    expect(String(fetcher.mock.calls[3][0])).toContain("/external-workers/agent-1/unarchive");
    expect(fetcher.mock.calls[3][1]).toMatchObject({
      method: "POST",
      headers: {
        "Idempotency-Key": "unarchive-1",
        "X-CSRF-Token": "csrf-1",
      },
    });

    const cancelled = await client.cancelExternalWorker("binding-1", "agent-1", "run-1", "cancel-1");
    expect(cancelled.state).toBe("cancelled");
    expect(String(fetcher.mock.calls[4][0])).toContain("/runs/run-1/cancel");
    expect(String(fetcher.mock.calls[4][0])).not.toContain("/archive");
  });

  it("fails closed when list or archive projections leak credentials or mismatch identity", async () => {
    const fetcher = vi.fn<typeof fetch>()
      .mockResolvedValueOnce(jsonResponse({
        items: [{
          provider: "cursor_cloud",
          externalAgentId: "agent-1",
          state: "archived",
          createdAt: "now",
          updatedAt: "now",
        }],
      }))
      .mockResolvedValueOnce(jsonResponse({
        items: [{
          provider: "cursor_cloud",
          externalAgentId: "agent-1",
          state: "ready",
          workerUrl: "https://cursor.com/agents/agent-1?token=secret",
          createdAt: "now",
          updatedAt: "now",
        }],
      }))
      .mockResolvedValueOnce(jsonResponse({
        provider: "cursor_cloud",
        externalAgentId: "other-agent",
        repository: "org/repo",
        startingRef: "main",
        state: "archived",
        createdAt: "now",
        updatedAt: "now",
      }))
      .mockResolvedValueOnce(jsonResponse({
        provider: "cursor_cloud",
        externalAgentId: "agent-1",
        repository: "org/repo",
        startingRef: "main",
        state: "archived",
        createdAt: "now",
        updatedAt: "now",
      }));
    const client = new GrokPtahBrokerClient({
      baseUrl: "https://contextdesk.example",
      fetcher,
      csrfToken: "csrf-1",
    });

    await expect(client.listExternalWorkers("binding-1"))
      .rejects.toMatchObject({ code: "invalid_response" });
    await expect(client.listExternalWorkers("binding-1", { includeArchived: true }))
      .rejects.toMatchObject({ code: "invalid_response" });
    await expect(client.archiveExternalWorker("binding-1", "agent-1", "archive-1"))
      .rejects.toMatchObject({ code: "invalid_response" });
    await expect(client.unarchiveExternalWorker("binding-1", "agent-1", "unarchive-1"))
      .rejects.toMatchObject({ code: "invalid_response" });
    await expect(client.listExternalWorkers("binding-1", { limit: 0 }))
      .rejects.toMatchObject({ code: "invalid_request" });
  });

  it("enforces CSRF, idempotency, pagination, and archive eligibility on the shared helpers", async () => {
    const fetcher = vi.fn<typeof fetch>()
      .mockResolvedValueOnce(jsonResponse({
        items: [{
          provider: "cursor_cloud",
          externalAgentId: "agent-1",
          state: "ready",
          createdAt: "now",
          updatedAt: "now",
        }, {
          provider: "cursor_cloud",
          externalAgentId: "agent-2",
          state: "ready",
          createdAt: "now",
          updatedAt: "now",
        }],
      }))
      .mockResolvedValueOnce(jsonResponse({
        provider: "cursor_cloud",
        externalAgentId: "agent-1",
        repository: "org/repo",
        startingRef: "main",
        state: "archived",
        createdAt: "now",
        updatedAt: "now",
      }));
    const mutating = new GrokPtahBrokerClient({
      baseUrl: "https://contextdesk.example",
      fetcher,
    });
    await expect(mutating.archiveExternalWorker("binding-1", "agent-1", "archive-1"))
      .rejects.toMatchObject({ code: "csrf_required" });
    await expect(mutating.unarchiveExternalWorker("binding-1", "agent-1", "unarchive-1"))
      .rejects.toMatchObject({ code: "csrf_required" });

    const client = new GrokPtahBrokerClient({
      baseUrl: "https://contextdesk.example",
      fetcher,
      csrfToken: "csrf-1",
    });
    await expect(client.archiveExternalWorker("binding-1", "agent-1", "  "))
      .rejects.toMatchObject({ code: "idempotency_required" });
    await expect(client.listExternalWorkers("binding-1", { limit: 1 }))
      .rejects.toMatchObject({ code: "invalid_response" });
    await expect(client.followUpExternalWorker("binding-1", "agent-1", {
      requestId: "follow-up-1",
      prompt: "Re-check the focused candidate",
    }, "follow-up-1")).rejects.toMatchObject({ code: "invalid_request" });
    expect(fetcher.mock.calls).toHaveLength(2);
    expect(String(fetcher.mock.calls[1][0])).toContain("/external-workers/agent-1");
    expect(String(fetcher.mock.calls[1][0])).not.toContain("/runs");
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
