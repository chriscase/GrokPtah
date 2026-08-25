import { describe, expect, it, vi } from "vitest";
import {
  GrokPtahBrokerClient,
  GrokPtahBrokerError,
} from "./grokptahBrokerClient";

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

describe("GrokPtahBrokerClient", () => {
  it("uses opaque broker ids and browser credentials without a bearer token", async () => {
    const fetcher = vi.fn<typeof fetch>().mockResolvedValue(jsonResponse({ brokerRunId: "run/1" }));
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
      .mockResolvedValueOnce(jsonResponse({ approvalId: "approval-1" }))
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
});
