import { describe, expect, it, vi } from "vitest";
import {
  type GrokPtahBrokerApprovalRequest,
  GrokPtahBrokerClient,
  GrokPtahBrokerError,
  parseBrokerApproval,
  parseBrokerBinding,
  parseBrokerRun,
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
          'id: 4\ndata: {"kind":"event","brokerRunId":"run-1","seq":4,"ts":"now","update":{}}\n\n' +
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
          'id: 1\ndata: {"kind":"event","brokerRunId":"run-1","seq":1.5,"ts":"now","update":{}}\n\n',
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
